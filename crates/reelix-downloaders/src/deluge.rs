//! Deluge adapter (Web UI JSON-RPC at `{base}/json`, Deluge 2.x; 1.3 mostly works).
//!
//! Logs in with `auth.login`, connects the web UI to the first daemon if needed, and uses the
//! Label plugin for categories (`label.*`).

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::{ContentKind, Protocol};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

use crate::common::{self, CommonSettings, HttpSettings, de_priority, ok_text};
use crate::model::*;
use crate::torrent::{self, TorrentSource};
use crate::{BuildContext, ClientError, DownloadClient};

pub const IMPLEMENTATION: &str = "deluge";

#[derive(Debug, Clone, Deserialize)]
pub struct DelugeSettings {
    #[serde(flatten)]
    pub common: CommonSettings,
    #[serde(flatten)]
    pub http: HttpSettings,
    pub password: String,
    pub movie_category: String,
    pub tv_category: String,
    /// Ebooks and audiobooks; empty = the movie category.
    #[serde(default)]
    pub book_category: String,
    /// Empty = the movie category.
    #[serde(default)]
    pub music_category: String,
    #[serde(deserialize_with = "de_priority")]
    pub recent_priority: Priority,
    #[serde(deserialize_with = "de_priority")]
    pub older_priority: Priority,
    #[serde(default)]
    pub add_paused: bool,
}

pub fn schema() -> ProviderSchema {
    let mut fields = vec![common::f_name("Deluge"), common::f_host(), common::f_port(8112)];
    fields.extend(common::f_ssl());
    fields.push(common::f_url_base(""));
    fields.push(
        common::f_password(Some("deluge"))
            .required()
            .help("Web UI password"),
    );
    fields.extend(common::f_categories("movies-reelix", "tv-reelix", "label"));
    fields.extend(common::f_extra_categories("label"));
    fields.extend(common::f_recent_older_priority());
    fields.push(FieldSchema::new("add_paused", "Add paused", FieldKind::Bool).default_value(false));
    fields.extend(common::f_common_tail());
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Deluge".into(),
        description: Some("Torrent client via the Deluge Web UI (Label plugin used for categories)".into()),
        info_url: Some("https://deluge-torrent.org/".into()),
        fields,
    }
}

pub const FIELDS: [&str; 16] = [
    "hash",
    "name",
    "state",
    "progress",
    "eta",
    "message",
    "is_finished",
    "save_path",
    "download_location",
    "total_size",
    "total_done",
    "ratio",
    "stop_ratio",
    "stop_at_ratio",
    "seeding_time",
    "completed_time",
];

#[derive(Debug, Clone, Deserialize)]
pub struct DeTorrent {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub eta: f64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub is_finished: bool,
    #[serde(default)]
    pub save_path: String,
    #[serde(default)]
    pub download_location: Option<String>,
    #[serde(default)]
    pub total_size: f64,
    #[serde(default)]
    pub total_done: f64,
    #[serde(default)]
    pub ratio: f64,
    #[serde(default)]
    pub stop_ratio: f64,
    #[serde(default)]
    pub stop_at_ratio: bool,
    #[serde(default)]
    pub seeding_time: Option<f64>,
    #[serde(default)]
    pub label: Option<String>,
    /// Unix seconds (Deluge 2); 0 until complete.
    #[serde(default)]
    pub completed_time: f64,
}

/// Deluge state → Reelix state.
pub fn map_state(state: &str, finished: bool) -> DownloadState {
    use DownloadState::*;
    match state {
        "Downloading" => Downloading,
        "Seeding" => Completed,
        "Paused" if finished => Completed,
        "Paused" => Paused,
        "Queued" if finished => Completed,
        "Queued" | "Checking" | "Allocating" => Queued,
        "Moving" => PostProcessing,
        "Error" => Warning,
        _ => Warning,
    }
}

pub fn to_item(t: DeTorrent, category: &str) -> DownloadItem {
    let state = map_state(&t.state, t.is_finished);
    let mut item = DownloadItem::new(t.hash.to_ascii_lowercase(), t.name.clone(), state);
    item.category = Some(category.to_owned());
    item.completed_at = (t.completed_time > 0.0).then_some(t.completed_time as i64);
    item.total_bytes = t.total_size.max(0.0) as u64;
    item.remaining_bytes = (t.total_size - t.total_done).max(0.0) as u64;
    item.eta_secs = (t.eta > 0.0).then_some(t.eta as u64);
    let dir = t
        .download_location
        .clone()
        .filter(|d| !d.is_empty())
        .unwrap_or(t.save_path.clone());
    item.output_path = Some(format!("{}/{}", dir.trim_end_matches(['/', '\\']), t.name));
    if state == DownloadState::Warning || (!t.message.is_empty() && t.message != "OK") {
        item.message = Some(t.message.clone()).filter(|m| !m.is_empty());
    }
    item.seed_ratio = Some(t.ratio);
    item.seeding_time = t.seeding_time.map(|s| s.max(0.0) as u64);
    let done = t.state == "Paused" && t.is_finished && (!t.stop_at_ratio || t.ratio >= t.stop_ratio);
    item.can_be_removed = done;
    item.can_move_files = done;
    item
}

/// Deluge Web client.
pub struct Deluge {
    http: reqwest::Client,
    fetch: reqwest::Client,
    url: String,
    settings: DelugeSettings,
    info: ClientInfo,
    cookie: Mutex<Option<String>>,
    next_id: AtomicU64,
}

/// Deluge JSON-RPC error code for "not authenticated".
const NOT_AUTHENTICATED: i64 = 1;

impl Deluge {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ClientError> {
        let s: DelugeSettings = common::decode_settings(&schema(), settings)?;
        Ok(Self {
            http: s.http.client(ctx)?,
            fetch: torrent::no_redirect_client(),
            url: format!("{}/json", s.http.base_url()?),
            info: s.common.info(IMPLEMENTATION),
            settings: s,
            cookie: Mutex::new(None),
            next_id: AtomicU64::new(1),
        })
    }

    /// Raw call; returns `(result, error)` envelope parts.
    async fn raw(&self, method: &str, params: &Value) -> Result<(Value, Option<(i64, String)>), ClientError> {
        let body =
            json!({ "method": method, "params": params, "id": self.next_id.fetch_add(1, Ordering::Relaxed) });
        let mut rb = self.http.post(&self.url).json(&body);
        if let Some(c) = self.cookie.lock().await.clone() {
            rb = rb.header(reqwest::header::COOKIE, c);
        }
        let resp = rb.send().await?;
        if let Some(c) = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|c| {
                c.split(';')
                    .next()
                    .filter(|kv| kv.trim_start().starts_with("_session_id="))
            })
        {
            *self.cookie.lock().await = Some(c.trim().to_owned());
        }
        let v: Value = serde_json::from_str(&ok_text(resp).await?)?;
        let err = v.get("error").filter(|e| !e.is_null()).map(|e| {
            (
                e.get("code").and_then(Value::as_i64).unwrap_or(0),
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned(),
            )
        });
        Ok((v.get("result").cloned().unwrap_or(Value::Null), err))
    }

    async fn login(&self) -> Result<(), ClientError> {
        let (res, err) = self.raw("auth.login", &json!([self.settings.password])).await?;
        if let Some((_, m)) = err {
            return Err(ClientError::Api(m));
        }
        if res.as_bool() != Some(true) {
            return Err(ClientError::Auth("invalid Deluge Web UI password".into()));
        }
        // Make sure the web UI is attached to a daemon.
        let (connected, _) = self.raw("web.connected", &json!([])).await?;
        if connected.as_bool() != Some(true) {
            let (hosts, _) = self.raw("web.get_hosts", &json!([])).await?;
            let host_id = hosts
                .as_array()
                .and_then(|h| h.first())
                .and_then(|h| h.get(0))
                .and_then(Value::as_str)
                .ok_or_else(|| ClientError::Api("Deluge Web UI has no daemon configured".into()))?
                .to_owned();
            self.raw("web.connect", &json!([host_id])).await?;
        }
        Ok(())
    }

    /// Authenticated call (logs in on demand).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        if self.cookie.lock().await.is_none() {
            self.login().await?;
        }
        for attempt in 0..2 {
            let (res, err) = self.raw(method, &params).await?;
            match err {
                None => return Ok(res),
                Some((NOT_AUTHENTICATED, _)) if attempt == 0 => self.login().await?,
                Some((NOT_AUTHENTICATED, m)) => return Err(ClientError::Auth(m)),
                Some((_, m)) => return Err(ClientError::Api(m)),
            }
        }
        Err(ClientError::Auth("not authenticated".into()))
    }

    fn categories_cfg(&self) -> Vec<String> {
        // Deluge labels are lowercase.
        common::configured_categories_all(&[
            &self.settings.movie_category,
            &self.settings.tv_category,
            &self.settings.book_category,
            &self.settings.music_category,
        ])
        .into_iter()
        .map(|c| c.to_ascii_lowercase())
        .collect()
    }

    async fn label_enabled(&self) -> Result<bool, ClientError> {
        let plugins = self.call("core.get_enabled_plugins", json!([])).await?;
        Ok(plugins
            .as_array()
            .is_some_and(|p| p.iter().any(|x| x.as_str() == Some("Label"))))
    }

    async fn labels(&self) -> Result<Vec<String>, ClientError> {
        let v = self.call("label.get_labels", json!([])).await?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
            .unwrap_or_default())
    }

    async fn run_checks(&self) -> (TestReport, Option<ClientError>) {
        let mut r = TestReport::default();
        match self.login().await {
            Ok(()) => {
                r.pass("connect");
                r.pass("auth");
            }
            Err(e @ ClientError::Auth(_)) => {
                r.pass("connect");
                r.fail("auth", e.to_string());
                return (r, Some(e));
            }
            Err(e) => {
                r.fail("connect", e.to_string());
                return (r, Some(e));
            }
        }
        match self.call("daemon.info", json!([])).await {
            Ok(_) => r.pass("version"),
            Err(e) => {
                r.fail("version", e.to_string());
                return (r, Some(e));
            }
        }
        match self.label_enabled().await {
            Ok(true) => r.pass("categories"),
            Ok(false) => {
                let m = "the Label plugin is not enabled in Deluge".to_owned();
                r.fail("categories", m.clone());
                return (r, Some(ClientError::InvalidSettings(m)));
            }
            Err(e) => {
                r.fail("categories", e.to_string());
                return (r, Some(e));
            }
        }
        (r, None)
    }
}

#[async_trait]
impl DownloadClient for Deluge {
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
        let mut options = json!({ "add_paused": paused });
        if let Some(r) = opts.seed_ratio {
            options["stop_at_ratio"] = true.into();
            options["stop_ratio"] = r.into();
        }
        let res = match &source {
            TorrentSource::Magnet { uri, .. } => {
                self.call("core.add_torrent_magnet", json!([uri, options])).await
            }
            TorrentSource::File { bytes, name, .. } => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                self.call("core.add_torrent_file", json!([name, b64, options]))
                    .await
            }
        };
        let hash = match res {
            Ok(Value::String(h)) => h.to_ascii_lowercase(),
            // Deluge returns null (or an "already in session" error) for duplicates.
            Ok(_) => source.hash().to_owned(),
            Err(ClientError::Api(m)) if m.to_ascii_lowercase().contains("already") => {
                source.hash().to_owned()
            }
            Err(e) => return Err(e),
        };
        if let Some(cat) = common::category_for(
            opts.category.as_deref(),
            opts.media_kind,
            &self.settings.movie_category,
            &self.settings.tv_category,
        ) {
            let cat = cat.to_ascii_lowercase();
            if !self.labels().await?.contains(&cat) {
                self.call("label.add", json!([cat])).await?;
            }
            self.call("label.set_torrent", json!([hash, cat])).await?;
        }
        let id = DownloadId(hash);
        if matches!(prio, Priority::High | Priority::Force) {
            self.call("core.queue_top", json!([[id.as_str()]])).await?;
        }
        Ok(id)
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let mut out = Vec::new();
        for cat in self.categories_cfg() {
            let res = self
                .call("core.get_torrents_status", json!([{ "label": cat }, FIELDS]))
                .await?;
            let Some(map) = res.as_object() else { continue };
            for (hash, v) in map {
                let mut t: DeTorrent = serde_json::from_value(v.clone())?;
                if t.hash.is_empty() {
                    t.hash = hash.clone();
                }
                out.push(to_item(t, &cat));
            }
        }
        Ok(out)
    }

    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError> {
        self.call("core.remove_torrent", json!([id.as_str(), delete_data]))
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
        let version = self
            .call("daemon.info", json!([]))
            .await?
            .as_str()
            .map(str::to_owned);
        let dir = self
            .call("core.get_config_value", json!(["download_location"]))
            .await?;
        let mut warnings = Vec::new();
        if !self.label_enabled().await? {
            warnings.push(HealthWarning::new(
                HealthKind::Misconfigured,
                "The Deluge Label plugin is disabled; Reelix cannot track its downloads",
            ));
        } else {
            let have = self.labels().await?;
            for c in crate::sabnzbd::health::missing_categories(&have, &self.categories_cfg()) {
                warnings.push(HealthWarning::new(
                    HealthKind::CategoryMissing,
                    format!("Label `{c}` does not exist yet; it is created on the first grab"),
                ));
            }
        }
        let output_dirs = dir
            .as_str()
            .filter(|d| !d.is_empty())
            .map(|d| {
                vec![OutputDir {
                    category: None,
                    path: d.trim_end_matches('/').to_owned(),
                }]
            })
            .unwrap_or_default();
        Ok(ClientStatus {
            version,
            output_dirs,
            warnings,
        })
    }

    async fn categories(&self) -> Result<Vec<String>, ClientError> {
        self.labels().await
    }

    async fn set_priority(&self, id: &DownloadId, priority: Priority) -> Result<(), ClientError> {
        let method = match priority {
            Priority::Paused => return self.pause(id).await,
            Priority::High | Priority::Force => "core.queue_top",
            Priority::Low => "core.queue_bottom",
            Priority::Default | Priority::Normal => return Ok(()),
        };
        self.call(method, json!([[id.as_str()]])).await.map(|_| ())
    }

    async fn pause(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.call("core.pause_torrent", json!([[id.as_str()]]))
            .await
            .map(|_| ())
    }

    async fn resume(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.call("core.resume_torrent", json!([[id.as_str()]]))
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_table() {
        use DownloadState::*;
        for (s, f, want) in [
            ("Downloading", false, Downloading),
            ("Seeding", true, Completed),
            ("Paused", false, Paused),
            ("Paused", true, Completed),
            ("Queued", false, Queued),
            ("Queued", true, Completed),
            ("Checking", false, Queued),
            ("Allocating", false, Queued),
            ("Moving", true, PostProcessing),
            ("Error", false, Warning),
        ] {
            assert_eq!(map_state(s, f), want, "{s}");
        }
    }
}
