//! Transmission adapter (RPC at `{base}/rpc`, default url_base `/transmission`).
//!
//! Handles the `409 Conflict` + `X-Transmission-Session-Id` handshake. Transmission has no
//! categories, so Reelix puts each category in its own download folder
//! (`{directory or default}/{category}`) and, on RPC 16+ (Transmission 4), also sets it as a label.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::{ContentKind, Protocol};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::common::{self, CommonSettings, HttpSettings, de_priority, ok_text};
use crate::model::*;
use crate::torrent::{self, TorrentSource};
use crate::{BuildContext, ClientError, DownloadClient};

pub const IMPLEMENTATION: &str = "transmission";
/// RPC version 14 = Transmission 2.80.
pub const MIN_RPC_VERSION: i64 = 14;
const SESSION_HEADER: &str = "X-Transmission-Session-Id";

#[derive(Debug, Clone, Deserialize)]
pub struct TransmissionSettings {
    #[serde(flatten)]
    pub common: CommonSettings,
    #[serde(flatten)]
    pub http: HttpSettings,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    pub movie_category: String,
    pub tv_category: String,
    /// Ebooks and audiobooks; empty = the movie category.
    #[serde(default)]
    pub book_category: String,
    /// Empty = the movie category.
    #[serde(default)]
    pub music_category: String,
    /// Base download folder; empty = Transmission's default `download-dir`.
    #[serde(default)]
    pub directory: String,
    #[serde(deserialize_with = "de_priority")]
    pub recent_priority: Priority,
    #[serde(deserialize_with = "de_priority")]
    pub older_priority: Priority,
    #[serde(default)]
    pub add_paused: bool,
}

pub fn schema() -> ProviderSchema {
    let mut fields = vec![
        common::f_name("Transmission"),
        common::f_host(),
        common::f_port(9091),
    ];
    fields.extend(common::f_ssl());
    fields.push(common::f_url_base("/transmission/"));
    fields.push(common::f_username(None));
    fields.push(common::f_password(None));
    fields.extend(common::f_categories("movies-reelix", "tv-reelix", "category"));
    fields.extend(common::f_extra_categories("category"));
    fields.push(
        FieldSchema::new("directory", "Directory", FieldKind::Path)
            .help("Base download folder; the category is appended as a subfolder. Empty = Transmission default.")
            .advanced(),
    );
    fields.extend(common::f_recent_older_priority());
    fields.push(FieldSchema::new("add_paused", "Add paused", FieldKind::Bool).default_value(false));
    fields.extend(common::f_common_tail());
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Transmission".into(),
        description: Some("Torrent client (Transmission 2.80 or newer)".into()),
        info_url: Some("https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md".into()),
        fields,
    }
}

/// `torrent-get` fields requested.
pub const FIELDS: [&str; 18] = [
    "id",
    "hashString",
    "name",
    "downloadDir",
    "totalSize",
    "leftUntilDone",
    "eta",
    "status",
    "error",
    "errorString",
    "isFinished",
    "uploadRatio",
    "seedRatioLimit",
    "seedRatioMode",
    "secondsSeeding",
    "labels",
    "percentDone",
    "doneDate",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrTorrent {
    pub hash_string: String,
    pub name: String,
    #[serde(default)]
    pub download_dir: String,
    #[serde(default)]
    pub total_size: i64,
    #[serde(default)]
    pub left_until_done: i64,
    #[serde(default)]
    pub eta: i64,
    #[serde(default)]
    pub status: i64,
    #[serde(default)]
    pub error: i64,
    #[serde(default)]
    pub error_string: String,
    #[serde(default)]
    pub is_finished: bool,
    #[serde(default)]
    pub upload_ratio: f64,
    #[serde(default)]
    pub seconds_seeding: Option<i64>,
    #[serde(default)]
    pub labels: Vec<String>,
    /// Unix seconds; 0 until complete.
    #[serde(default)]
    pub done_date: i64,
}

/// Transmission status code (+ error code) → Reelix state.
pub fn map_state(status: i64, error: i64, left: i64, finished: bool) -> DownloadState {
    use DownloadState::*;
    if error == 3 {
        return Warning; // local error
    }
    match status {
        0 if left == 0 || finished => Completed,
        0 => Paused,
        1..=3 => Queued, // check wait / checking / download wait
        4 if error == 2 => Warning,
        4 => Downloading,
        5 | 6 => Completed, // seed wait / seeding
        _ => Warning,
    }
}

pub fn to_item(t: TrTorrent, category: &str) -> DownloadItem {
    let state = map_state(t.status, t.error, t.left_until_done, t.is_finished);
    let mut item = DownloadItem::new(t.hash_string.to_ascii_lowercase(), t.name.clone(), state);
    item.category = Some(category.to_owned());
    item.completed_at = (t.done_date > 0).then_some(t.done_date);
    item.total_bytes = t.total_size.max(0) as u64;
    item.remaining_bytes = t.left_until_done.max(0) as u64;
    item.eta_secs = (t.eta >= 0).then_some(t.eta as u64);
    item.output_path = Some(format!(
        "{}/{}",
        t.download_dir.trim_end_matches(['/', '\\']),
        t.name
    ));
    if t.error != 0 && !t.error_string.is_empty() {
        item.message = Some(t.error_string);
    }
    item.seed_ratio = Some(t.upload_ratio);
    item.seeding_time = t.seconds_seeding.map(|s| s.max(0) as u64);
    // Stopped after reaching its seed limit.
    let done = t.status == 0 && t.is_finished;
    item.can_be_removed = done;
    item.can_move_files = done;
    item
}

/// Transmission client.
pub struct Transmission {
    http: reqwest::Client,
    fetch: reqwest::Client,
    url: String,
    settings: TransmissionSettings,
    info: ClientInfo,
    session: Mutex<Option<String>>,
}

impl Transmission {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ClientError> {
        let s: TransmissionSettings = common::decode_settings(&schema(), settings)?;
        Ok(Self {
            http: s.http.client(ctx)?,
            fetch: torrent::no_redirect_client(),
            url: format!("{}/rpc", s.http.base_url()?),
            info: s.common.info(IMPLEMENTATION),
            settings: s,
            session: Mutex::new(None),
        })
    }

    /// One RPC call; refreshes the session id on 409 and retries.
    pub async fn rpc(&self, method: &str, arguments: Value) -> Result<Value, ClientError> {
        let body = json!({ "method": method, "arguments": arguments });
        for _ in 0..3 {
            let mut rb = self.http.post(&self.url).json(&body);
            if let Some(u) = self.settings.username.as_deref().filter(|u| !u.is_empty()) {
                rb = rb.basic_auth(u, self.settings.password.as_deref());
            }
            if let Some(id) = self.session.lock().await.clone() {
                rb = rb.header(SESSION_HEADER, id);
            }
            let resp = rb.send().await?;
            if resp.status().as_u16() == 409 {
                let id = resp
                    .headers()
                    .get(SESSION_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
                    .ok_or_else(|| ClientError::Parse("409 without session id".into()))?;
                *self.session.lock().await = Some(id);
                continue;
            }
            let text = ok_text(resp).await?;
            let v: Value = serde_json::from_str(&text)?;
            let result = v.get("result").and_then(Value::as_str).unwrap_or("");
            if result != "success" {
                return Err(ClientError::Api(result.to_owned()));
            }
            return Ok(v.get("arguments").cloned().unwrap_or(Value::Null));
        }
        Err(ClientError::Api("session id handshake failed".into()))
    }

    async fn session_get(&self) -> Result<Value, ClientError> {
        self.rpc("session-get", json!({})).await
    }

    fn categories_cfg(&self) -> Vec<String> {
        common::configured_categories_all(&[
            &self.settings.movie_category,
            &self.settings.tv_category,
            &self.settings.book_category,
            &self.settings.music_category,
        ])
    }

    /// Download folder for a category.
    fn dir_for(base: &str, category: &str) -> String {
        format!("{}/{}", base.trim_end_matches(['/', '\\']), category)
    }

    async fn base_dir(&self, session: Option<&Value>) -> Result<String, ClientError> {
        if !self.settings.directory.trim().is_empty() {
            return Ok(self.settings.directory.trim().to_owned());
        }
        let s = match session {
            Some(s) => s.clone(),
            None => self.session_get().await?,
        };
        Ok(s.get("download-dir")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned())
    }

    fn rpc_version(session: &Value) -> i64 {
        session.get("rpc-version").and_then(Value::as_i64).unwrap_or(0)
    }

    async fn run_checks(&self) -> (TestReport, Option<ClientError>) {
        let mut r = TestReport::default();
        match self.session_get().await {
            Ok(s) => {
                r.pass("connect");
                r.pass("auth");
                let v = Self::rpc_version(&s);
                if v >= MIN_RPC_VERSION {
                    r.pass("version");
                } else {
                    let e = ClientError::UnsupportedVersion {
                        found: format!("RPC {v}"),
                        minimum: format!("RPC {MIN_RPC_VERSION}"),
                    };
                    r.fail("version", e.to_string());
                    return (r, Some(e));
                }
                (r, None)
            }
            Err(e @ ClientError::Auth(_)) => {
                r.pass("connect");
                r.fail("auth", e.to_string());
                (r, Some(e))
            }
            Err(e) => {
                r.fail("connect", e.to_string());
                (r, Some(e))
            }
        }
    }

    async fn ids_action(&self, method: &str, id: &DownloadId) -> Result<(), ClientError> {
        self.rpc(method, json!({ "ids": [id.as_str()] }))
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl DownloadClient for Transmission {
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }

    fn info(&self) -> &ClientInfo {
        &self.info
    }

    fn category_for_kind(&self, kind: ContentKind) -> Option<String> {
        let s = &self.settings;
        common::category_for_kind(
            kind,
            &s.movie_category,
            &s.tv_category,
            &s.book_category,
            &s.music_category,
        )
    }

    async fn add(&self, release: &ReleasePayload, opts: &AddOptions) -> Result<DownloadId, ClientError> {
        let source = torrent::resolve(&self.fetch, release, &opts.title).await?;
        let session = self.session_get().await?;
        let category = common::category_for(
            opts.category.as_deref(),
            opts.media_kind,
            &self.settings.movie_category,
            &self.settings.tv_category,
        );
        let prio = opts.priority.unwrap_or(if opts.is_recent {
            self.settings.recent_priority
        } else {
            self.settings.older_priority
        });
        let paused = opts.add_paused || self.settings.add_paused || prio == Priority::Paused;
        let mut args = json!({ "paused": paused });
        match &source {
            TorrentSource::Magnet { uri, .. } => args["filename"] = uri.clone().into(),
            TorrentSource::File { bytes, .. } => {
                use base64::Engine;
                args["metainfo"] = base64::engine::general_purpose::STANDARD.encode(bytes).into();
            }
        }
        if let Some(c) = &category {
            let base = self.base_dir(Some(&session)).await?;
            if !base.is_empty() {
                args["download-dir"] = Self::dir_for(&base, c).into();
            }
            if Self::rpc_version(&session) >= 16 {
                args["labels"] = json!([c]);
            }
        }
        if matches!(prio, Priority::High | Priority::Force) {
            args["bandwidthPriority"] = 1.into();
        } else if prio == Priority::Low {
            args["bandwidthPriority"] = (-1).into();
        }
        let res = self.rpc("torrent-add", args).await?;
        let added = res
            .get("torrent-added")
            .or_else(|| res.get("torrent-duplicate"))
            .ok_or_else(|| ClientError::Rejected("Transmission did not add the torrent".into()))?;
        let hash = added
            .get("hashString")
            .and_then(Value::as_str)
            .map_or_else(|| source.hash().to_owned(), str::to_ascii_lowercase);
        let id = DownloadId(hash);
        if opts.seed_ratio.is_some() || opts.seed_time_minutes.is_some() {
            let mut set = json!({ "ids": [id.as_str()] });
            if let Some(r) = opts.seed_ratio {
                set["seedRatioLimit"] = r.into();
                set["seedRatioMode"] = 1.into();
            }
            if let Some(m) = opts.seed_time_minutes {
                set["seedIdleLimit"] = m.into();
                set["seedIdleMode"] = 1.into();
            }
            self.rpc("torrent-set", set).await?;
        }
        if prio == Priority::Force && !paused {
            self.ids_action("torrent-start-now", &id).await?;
        }
        Ok(id)
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let session = self.session_get().await?;
        let base = self.base_dir(Some(&session)).await?;
        let res = self.rpc("torrent-get", json!({ "fields": FIELDS })).await?;
        let list: Vec<TrTorrent> = serde_json::from_value(res.get("torrents").cloned().unwrap_or(json!([])))?;
        let cats = self.categories_cfg();
        let mut out = Vec::new();
        for t in list {
            let dir = t.download_dir.trim_end_matches(['/', '\\']).to_owned();
            let cat = cats.iter().find(|c| {
                t.labels.iter().any(|l| l.eq_ignore_ascii_case(c))
                    || (!base.is_empty() && dir == Self::dir_for(&base, c))
                    || dir.rsplit(['/', '\\']).next() == Some(c.as_str())
            });
            if let Some(c) = cat.cloned() {
                out.push(to_item(t, &c));
            }
        }
        Ok(out)
    }

    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError> {
        self.rpc(
            "torrent-remove",
            json!({ "ids": [id.as_str()], "delete-local-data": delete_data }),
        )
        .await
        .map(|_| ())
    }

    async fn test(&self) -> Result<(), ClientError> {
        self.run_checks().await.1.map_or(Ok(()), Err)
    }

    async fn test_report(&self) -> TestReport {
        self.run_checks().await.0
    }

    async fn status(&self) -> Result<ClientStatus, ClientError> {
        let s = self.session_get().await?;
        let version = s.get("version").and_then(Value::as_str).map(str::to_owned);
        let mut warnings = Vec::new();
        if Self::rpc_version(&s) < MIN_RPC_VERSION {
            warnings.push(HealthWarning::new(
                HealthKind::VersionTooOld,
                "Transmission is too old; Reelix needs 2.80 or newer",
            ));
        }
        let base = self.base_dir(Some(&s)).await?;
        let mut output_dirs = Vec::new();
        if !base.is_empty() {
            output_dirs.push(OutputDir {
                category: None,
                path: base.trim_end_matches('/').to_owned(),
            });
            for c in self.categories_cfg() {
                output_dirs.push(OutputDir {
                    path: Self::dir_for(&base, &c),
                    category: Some(c),
                });
            }
        }
        Ok(ClientStatus {
            version,
            output_dirs,
            warnings,
        })
    }

    async fn set_priority(&self, id: &DownloadId, priority: Priority) -> Result<(), ClientError> {
        match priority {
            Priority::Paused => self.pause(id).await,
            Priority::Force => {
                self.ids_action("queue-move-top", id).await?;
                self.ids_action("torrent-start-now", id).await
            }
            Priority::High => self.ids_action("queue-move-top", id).await,
            Priority::Low => self.ids_action("queue-move-bottom", id).await,
            Priority::Default | Priority::Normal => Ok(()),
        }
    }

    async fn pause(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.ids_action("torrent-stop", id).await
    }

    async fn resume(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.ids_action("torrent-start", id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_table() {
        use DownloadState::*;
        let cases = [
            ((0, 0, 100, false), Paused),
            ((0, 0, 0, true), Completed),
            ((1, 0, 100, false), Queued),
            ((2, 0, 100, false), Queued),
            ((3, 0, 100, false), Queued),
            ((4, 0, 100, false), Downloading),
            ((4, 2, 100, false), Warning),
            ((4, 3, 100, false), Warning),
            ((5, 0, 0, false), Completed),
            ((6, 0, 0, false), Completed),
            ((9, 0, 0, false), Warning),
        ];
        for ((s, e, l, f), want) in cases {
            assert_eq!(map_state(s, e, l, f), want, "{s}/{e}");
        }
    }
}
