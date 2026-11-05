//! rTorrent / ruTorrent adapter (XML-RPC over HTTP, default path `/RPC2`).
//!
//! The category is stored in `d.custom1` (ruTorrent's label). Download ids are uppercase
//! info hashes as rTorrent reports them; lookups are case-insensitive.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::{ContentKind, Protocol};
use serde::Deserialize;
use serde_json::Value;

use crate::common::{self, CommonSettings, HttpSettings, de_priority, ok_text};
use crate::model::*;
use crate::torrent::{self, TorrentSource};
use crate::xmlrpc::{XValue, decode_response, encode_call};
use crate::{BuildContext, ClientError, DownloadClient};

pub const IMPLEMENTATION: &str = "rtorrent";
pub const MIN_VERSION: &str = "0.9.0";

#[derive(Debug, Clone, Deserialize)]
pub struct RTorrentSettings {
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
    /// Download folder; empty = rTorrent default.
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
    let mut fields = vec![common::f_name("rTorrent"), common::f_host(), common::f_port(8080)];
    fields.extend(common::f_ssl());
    fields.push(
        common::f_url_base("/RPC2").help("XML-RPC endpoint path (ruTorrent: /RPC2 or /plugins/rpc/rpc.php)"),
    );
    fields.push(common::f_username(None));
    fields.push(common::f_password(None));
    fields.extend(common::f_categories("movies-reelix", "tv-reelix", "label"));
    fields.extend(common::f_extra_categories("label"));
    fields.push(
        FieldSchema::new("directory", "Directory", FieldKind::Path)
            .help("Download folder; empty = rTorrent default")
            .advanced(),
    );
    fields.extend(common::f_recent_older_priority());
    fields.push(FieldSchema::new("add_paused", "Add paused", FieldKind::Bool).default_value(false));
    fields.extend(common::f_common_tail());
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "rTorrent".into(),
        description: Some("rTorrent / ruTorrent via XML-RPC".into()),
        info_url: Some("https://github.com/rakshasa/rtorrent/wiki".into()),
        fields,
    }
}

/// Columns requested from `d.multicall2`, in order.
pub const COLUMNS: [&str; 14] = [
    "d.hash=",
    "d.name=",
    "d.custom1=",
    "d.size_bytes=",
    "d.left_bytes=",
    "d.down.rate=",
    "d.ratio=",
    "d.is_open=",
    "d.is_active=",
    "d.complete=",
    "d.hashing=",
    "d.base_path=",
    "d.message=",
    "d.timestamp.finished=",
];

/// One `d.multicall2` row.
#[derive(Debug, Clone, PartialEq)]
pub struct RtTorrent {
    pub hash: String,
    pub name: String,
    pub label: String,
    pub size: i64,
    pub left: i64,
    pub down_rate: i64,
    /// Ratio × 1000.
    pub ratio: i64,
    pub is_open: bool,
    pub is_active: bool,
    pub complete: bool,
    pub hashing: bool,
    pub base_path: String,
    pub message: String,
    /// When the download finished (Unix seconds), 0 while it is not finished.
    pub finished: i64,
}

impl RtTorrent {
    pub fn from_row(row: &[XValue]) -> Option<Self> {
        let s = |i: usize| row.get(i).and_then(XValue::as_str).unwrap_or("").to_owned();
        let n = |i: usize| row.get(i).and_then(XValue::as_i64).unwrap_or(0);
        Some(Self {
            hash: row.first()?.as_str()?.to_owned(),
            name: s(1),
            label: url_decode(&s(2)),
            size: n(3),
            left: n(4),
            down_rate: n(5),
            ratio: n(6),
            is_open: n(7) != 0,
            is_active: n(8) != 0,
            complete: n(9) != 0,
            hashing: n(10) != 0,
            base_path: s(11),
            message: s(12),
            finished: n(13),
        })
    }
}

/// ruTorrent URL-encodes labels in `custom1`.
fn url_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| s.to_owned())
}

/// rTorrent flags → Reelix state.
pub fn map_state(t: &RtTorrent) -> DownloadState {
    use DownloadState::*;
    if t.hashing {
        Queued
    } else if t.complete {
        Completed
    } else if !t.message.is_empty() && t.message.to_ascii_lowercase().contains("error") {
        Warning
    } else if !t.is_active {
        Paused
    } else if t.down_rate > 0 || t.is_open {
        Downloading
    } else {
        Queued
    }
}

pub fn to_item(t: RtTorrent) -> DownloadItem {
    let state = map_state(&t);
    let mut item = DownloadItem::new(t.hash.clone(), t.name.clone(), state);
    item.category = Some(t.label.clone()).filter(|l| !l.is_empty());
    item.total_bytes = t.size.max(0) as u64;
    item.remaining_bytes = t.left.max(0) as u64;
    item.eta_secs = (t.down_rate > 0 && t.left > 0).then(|| (t.left / t.down_rate) as u64);
    item.output_path = Some(t.base_path.clone()).filter(|p| !p.is_empty());
    item.message = Some(t.message.clone()).filter(|m| !m.is_empty());
    item.seed_ratio = Some(t.ratio as f64 / 1000.0);
    // Without this, a finished torrent looks never-completed to the monitor, so it is adopted again once
    // its tracked row is pruned after 30 days.
    item.completed_at = (t.complete && t.finished > 0).then_some(t.finished);
    // rTorrent stops (closes) torrents itself once its ratio group limit is reached.
    let done = t.complete && !t.is_active;
    item.can_be_removed = done;
    item.can_move_files = done;
    item
}

/// rTorrent client.
pub struct RTorrent {
    http: reqwest::Client,
    fetch: reqwest::Client,
    url: String,
    settings: RTorrentSettings,
    info: ClientInfo,
}

impl RTorrent {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ClientError> {
        let s: RTorrentSettings = common::decode_settings(&schema(), settings)?;
        Ok(Self {
            http: s.http.client(ctx)?,
            fetch: torrent::no_redirect_client(),
            url: s.http.base_url()?,
            info: s.common.info(IMPLEMENTATION),
            settings: s,
        })
    }

    pub async fn call(&self, method: &str, params: &[XValue]) -> Result<XValue, ClientError> {
        let mut rb = self
            .http
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "text/xml")
            .body(encode_call(method, params));
        if let Some(u) = self.settings.username.as_deref().filter(|u| !u.is_empty()) {
            rb = rb.basic_auth(u, self.settings.password.as_deref());
        }
        decode_response(&ok_text(rb.send().await?).await?)
    }

    async fn version(&self) -> Result<String, ClientError> {
        Ok(self
            .call("system.client_version", &[])
            .await?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    fn categories_cfg(&self) -> Vec<String> {
        common::configured_categories_all(&[
            &self.settings.movie_category,
            &self.settings.tv_category,
            &self.settings.book_category,
            &self.settings.music_category,
        ])
    }

    async fn list(&self) -> Result<Vec<RtTorrent>, ClientError> {
        let mut params: Vec<XValue> = vec!["".into(), "main".into()];
        params.extend(COLUMNS.iter().map(|c| XValue::from(*c)));
        let v = self.call("d.multicall2", &params).await?;
        Ok(v.as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.as_array().and_then(RtTorrent::from_row))
            .collect())
    }

    /// Finds the hash with rTorrent's own casing.
    async fn hash_of(&self, id: &DownloadId) -> Result<String, ClientError> {
        self.list()
            .await?
            .into_iter()
            .find(|t| t.hash.eq_ignore_ascii_case(id.as_str()))
            .map(|t| t.hash)
            .ok_or_else(|| ClientError::NotFound(id.0.clone()))
    }

    async fn d_call(&self, method: &str, id: &DownloadId, extra: &[XValue]) -> Result<(), ClientError> {
        let hash = self.hash_of(id).await?;
        let mut p = vec![XValue::from(hash)];
        p.extend_from_slice(extra);
        self.call(method, &p).await.map(|_| ())
    }
}

/// rTorrent `d.priority` value (0 off, 1 low, 2 normal, 3 high).
pub fn priority_value(p: Priority) -> i64 {
    match p {
        Priority::Low => 1,
        Priority::High | Priority::Force => 3,
        Priority::Paused | Priority::Default | Priority::Normal => 2,
    }
}

#[async_trait]
impl DownloadClient for RTorrent {
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
        let prio = opts.priority.unwrap_or(if opts.is_recent {
            self.settings.recent_priority
        } else {
            self.settings.older_priority
        });
        let paused = opts.add_paused || self.settings.add_paused || prio == Priority::Paused;
        let mut cmds: Vec<XValue> = vec!["".into()];
        cmds.push(match &source {
            TorrentSource::Magnet { uri, .. } => uri.clone().into(),
            TorrentSource::File { bytes, .. } => XValue::Base64(bytes.clone()),
        });
        if let Some(c) = common::category_for(
            opts.category.as_deref(),
            opts.media_kind,
            &self.settings.movie_category,
            &self.settings.tv_category,
        ) {
            let enc: String = url::form_urlencoded::byte_serialize(c.as_bytes()).collect();
            cmds.push(format!("d.custom1.set={enc}").into());
        }
        if !self.settings.directory.trim().is_empty() {
            cmds.push(format!("d.directory.set=\"{}\"", self.settings.directory.trim()).into());
        }
        cmds.push(format!("d.priority.set={}", priority_value(prio)).into());
        let method = match (&source, paused) {
            (TorrentSource::Magnet { .. }, false) => "load.start",
            (TorrentSource::Magnet { .. }, true) => "load.normal",
            (TorrentSource::File { .. }, false) => "load.raw_start",
            (TorrentSource::File { .. }, true) => "load.raw",
        };
        let res = self.call(method, &cmds).await?;
        if res.as_i64().is_some_and(|r| r != 0) {
            return Err(ClientError::Rejected("rTorrent refused the torrent".into()));
        }
        Ok(DownloadId(source.hash().to_ascii_uppercase()))
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let cats = self.categories_cfg();
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|t| cats.iter().any(|c| c.eq_ignore_ascii_case(&t.label)))
            .map(to_item)
            .collect())
    }

    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError> {
        if delete_data {
            // ruTorrent's erasedata plugin deletes files of torrents flagged in custom5.
            self.d_call("d.custom5.set", id, &["1".into()]).await?;
        }
        self.d_call("d.erase", id, &[]).await
    }

    async fn test(&self) -> Result<(), ClientError> {
        let v = self.version().await?;
        if !common::version_at_least(&v, MIN_VERSION) {
            return Err(ClientError::UnsupportedVersion {
                found: v,
                minimum: MIN_VERSION.into(),
            });
        }
        Ok(())
    }

    async fn test_report(&self) -> TestReport {
        let mut r = TestReport::default();
        match self.version().await {
            Ok(v) => {
                r.pass("connect");
                r.pass("auth");
                if common::version_at_least(&v, MIN_VERSION) {
                    r.pass("version");
                } else {
                    r.fail("version", format!("rTorrent {v} is older than {MIN_VERSION}"));
                }
            }
            Err(e @ ClientError::Auth(_)) => {
                r.pass("connect");
                r.fail("auth", e.to_string());
            }
            Err(e) => r.fail("connect", e.to_string()),
        }
        r
    }

    async fn status(&self) -> Result<ClientStatus, ClientError> {
        let version = self.version().await?;
        let mut warnings = Vec::new();
        if !common::version_at_least(&version, MIN_VERSION) {
            warnings.push(HealthWarning::new(
                HealthKind::VersionTooOld,
                format!("rTorrent {version} is not supported; please upgrade to {MIN_VERSION} or newer"),
            ));
        }
        let dir = if self.settings.directory.trim().is_empty() {
            self.call("directory.default", &[])
                .await
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        } else {
            self.settings.directory.trim().to_owned()
        };
        let output_dirs = if dir.is_empty() {
            Vec::new()
        } else {
            vec![OutputDir {
                category: None,
                path: dir.trim_end_matches('/').to_owned(),
            }]
        };
        Ok(ClientStatus {
            version: Some(version),
            output_dirs,
            warnings,
        })
    }

    async fn set_priority(&self, id: &DownloadId, priority: Priority) -> Result<(), ClientError> {
        if priority == Priority::Paused {
            return self.pause(id).await;
        }
        self.d_call("d.priority.set", id, &[XValue::Int(priority_value(priority))])
            .await
    }

    async fn pause(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.d_call("d.stop", id, &[]).await
    }

    async fn resume(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.d_call("d.start", id, &[]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(complete: bool, active: bool, open: bool, hashing: bool, msg: &str) -> RtTorrent {
        RtTorrent {
            hash: "AB".into(),
            name: "n".into(),
            label: "tv".into(),
            size: 10,
            left: if complete { 0 } else { 5 },
            down_rate: 0,
            ratio: 1500,
            is_open: open,
            is_active: active,
            complete,
            hashing,
            base_path: "/d/n".into(),
            message: msg.into(),
            finished: if complete { 1_700_000_000 } else { 0 },
        }
    }

    #[test]
    fn state_table() {
        use DownloadState::*;
        assert_eq!(map_state(&t(false, false, false, true, "")), Queued);
        assert_eq!(map_state(&t(true, true, true, false, "")), Completed);
        assert_eq!(map_state(&t(false, false, false, false, "")), Paused);
        assert_eq!(map_state(&t(false, true, true, false, "")), Downloading);
        assert_eq!(
            map_state(&t(false, true, true, false, "Tracker: [Error]")),
            Warning
        );
        let item = to_item(t(true, false, false, false, ""));
        assert!(item.can_be_removed);
        assert_eq!(item.completed_at, Some(1_700_000_000));
        assert_eq!(to_item(t(false, true, true, false, "")).completed_at, None);
        assert_eq!(item.seed_ratio, Some(1.5));
        assert_eq!(url_decode("tv%20shows"), "tv shows");
    }

    #[test]
    fn rows_without_the_finished_column_still_parse() {
        let row: Vec<XValue> = ["AB", "n", "tv"].iter().map(|v| XValue::from(*v)).collect();
        let t = RtTorrent::from_row(&row).unwrap();
        assert_eq!(t.finished, 0);
        assert_eq!(to_item(t).completed_at, None);
    }
}
