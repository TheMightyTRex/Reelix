//! NZBGet adapter (JSON-RPC at `{base}/jsonrpc`, HTTP basic auth). Supports NZBGet 16+.
//!
//! Download ids are NZBGet's numeric `NZBID`s. Queue (`listgroups`) and history (`history`)
//! are merged and filtered to Reelix's categories.

use async_trait::async_trait;
use reelix_core::provider::ProviderSchema;
use reelix_core::{ContentKind, Protocol};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::common::{self, CommonSettings, HttpSettings, de_priority, ok_text, version_at_least};
use crate::model::*;
use crate::{BuildContext, ClientError, DownloadClient};

pub const IMPLEMENTATION: &str = "nzbget";
pub const MIN_VERSION: &str = "16.0";

#[derive(Debug, Clone, Deserialize)]
pub struct NzbGetSettings {
    #[serde(flatten)]
    pub common: CommonSettings,
    #[serde(flatten)]
    pub http: HttpSettings,
    pub username: String,
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
    let mut fields = vec![common::f_name("NZBGet"), common::f_host(), common::f_port(6789)];
    fields.extend(common::f_ssl());
    fields.push(common::f_url_base(""));
    fields.push(common::f_username(Some("nzbget")).required());
    fields.push(common::f_password(Some("tegbzn6789")).required());
    fields.extend(common::f_categories("movies", "tv", "category"));
    fields.extend(common::f_extra_categories("category"));
    fields.extend(common::f_recent_older_priority());
    fields.push(
        reelix_core::provider::FieldSchema::new(
            "add_paused",
            "Add paused",
            reelix_core::provider::FieldKind::Bool,
        )
        .default_value(false),
    );
    fields.extend(common::f_common_tail());
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "NZBGet".into(),
        description: Some("Usenet downloader (version 16 or newer)".into()),
        info_url: Some("https://nzbget.com/documentation/api/".into()),
        fields,
    }
}

/// NZBGet priority number.
pub fn priority_value(p: Priority) -> i64 {
    match p {
        Priority::Default | Priority::Normal | Priority::Paused => 0,
        Priority::Low => -50,
        Priority::High => 50,
        Priority::Force => 900,
    }
}

fn priority_from(v: i64) -> Priority {
    match v {
        v if v >= 900 => Priority::Force,
        v if v > 0 => Priority::High,
        v if v < 0 => Priority::Low,
        _ => Priority::Normal,
    }
}

/// Queue group status → state.
pub fn queue_state(status: &str) -> DownloadState {
    match status {
        "QUEUED" | "FETCHING" => DownloadState::Queued,
        "PAUSED" => DownloadState::Paused,
        "DOWNLOADING" => DownloadState::Downloading,
        "PP_QUEUED" | "LOADING_PARS" | "VERIFYING_SOURCES" | "REPAIRING" | "VERIFYING_REPAIRED"
        | "RENAMING" | "UNPACKING" | "MOVING" | "EXECUTING_SCRIPT" | "PP_FINISHED" => {
            DownloadState::PostProcessing
        }
        _ => DownloadState::Queued,
    }
}

/// History entry → state and failure classification (from `Status` and the sub-statuses).
pub fn history_state(h: &HistoryEntry) -> (DownloadState, Option<UsenetFailure>, Option<String>) {
    let (main, sub) = h.status.split_once('/').unwrap_or((h.status.as_str(), ""));
    let failure = |f: UsenetFailure, msg: String| (DownloadState::Failed, Some(f), Some(msg));
    match main {
        "SUCCESS" => (DownloadState::Completed, None, None),
        "WARNING" if matches!(sub, "SCRIPT") => (
            DownloadState::Completed,
            None,
            Some("Post-processing script reported a warning".into()),
        ),
        "WARNING" => {
            let msg = format!("NZBGet warning: {sub}");
            match sub {
                "SPACE" => failure(UsenetFailure::DiskFull, "Not enough disk space".into()),
                "PASSWORD" => failure(
                    UsenetFailure::PasswordProtected,
                    "Archive is password protected".into(),
                ),
                "DAMAGED" | "REPAIRABLE" => failure(UsenetFailure::RepairFailed, msg),
                "HEALTH" => failure(UsenetFailure::MissingArticles, msg),
                _ => (DownloadState::Warning, None, Some(msg)),
            }
        }
        "DELETED" => match sub {
            "DUPE" | "COPY" => failure(UsenetFailure::Duplicate, "Deleted as duplicate".into()),
            "HEALTH" => failure(
                UsenetFailure::MissingArticles,
                "Deleted: health below critical (missing articles)".into(),
            ),
            "BAD" => failure(UsenetFailure::Other, "Marked bad".into()),
            "SCAN" => failure(UsenetFailure::FetchFailed, "NZB could not be parsed".into()),
            _ => failure(UsenetFailure::Other, "Deleted manually in NZBGet".into()),
        },
        "FAILURE" => {
            let f = match sub {
                "PAR" => UsenetFailure::RepairFailed,
                "UNPACK" if h.unpack_status == "PASSWORD" => UsenetFailure::PasswordProtected,
                "UNPACK" if h.unpack_status == "SPACE" => UsenetFailure::DiskFull,
                "UNPACK" => UsenetFailure::UnpackFailed,
                "HEALTH" => UsenetFailure::MissingArticles,
                "FETCH" | "SCAN" => UsenetFailure::FetchFailed,
                "MOVE" => UsenetFailure::DiskFull,
                _ => UsenetFailure::Other,
            };
            failure(f, format!("NZBGet failure: {sub}"))
        }
        _ => (
            DownloadState::Warning,
            None,
            Some(format!("Unknown NZBGet status {}", h.status)),
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct QueueGroup {
    #[serde(rename = "NZBID")]
    pub nzb_id: i64,
    #[serde(rename = "NZBName")]
    pub nzb_name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub file_size_lo: u64,
    #[serde(default)]
    pub file_size_hi: u64,
    #[serde(default)]
    pub remaining_size_lo: u64,
    #[serde(default)]
    pub remaining_size_hi: u64,
    #[serde(default)]
    pub paused_size_lo: u64,
    #[serde(default)]
    pub paused_size_hi: u64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub max_priority: i64,
    #[serde(default)]
    pub dest_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct HistoryEntry {
    #[serde(rename = "NZBID")]
    pub nzb_id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub file_size_lo: u64,
    #[serde(default)]
    pub file_size_hi: u64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub unpack_status: String,
    #[serde(default)]
    pub dest_dir: String,
    #[serde(default)]
    pub final_dir: String,
}

fn size(lo: u64, hi: u64) -> u64 {
    (hi << 32) | (lo & 0xFFFF_FFFF)
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<Value>,
}

/// NZBGet client.
pub struct NzbGet {
    http: reqwest::Client,
    url: String,
    settings: NzbGetSettings,
    info: ClientInfo,
    next_id: AtomicU64,
}

impl NzbGet {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ClientError> {
        let s: NzbGetSettings = common::decode_settings(&schema(), settings)?;
        Ok(Self {
            http: s.http.client(ctx)?,
            url: format!("{}/jsonrpc", s.http.base_url()?),
            info: s.common.info(IMPLEMENTATION),
            settings: s,
            next_id: AtomicU64::new(1),
        })
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.next_id.fetch_add(1, Ordering::Relaxed),
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.url)
            .basic_auth(&self.settings.username, Some(&self.settings.password))
            .json(&body)
            .send()
            .await?;
        let text = ok_text(resp).await?;
        let r: RpcResponse = serde_json::from_str(&text)?;
        if let Some(err) = r.error.filter(|e| !e.is_null()) {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| err.to_string(), str::to_owned);
            return Err(ClientError::Api(msg));
        }
        Ok(r.result)
    }

    async fn version(&self) -> Result<String, ClientError> {
        Ok(self
            .rpc("version", json!([]))
            .await?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    /// `config` as name → value pairs.
    async fn config(&self) -> Result<Vec<(String, String)>, ClientError> {
        let v = self.rpc("config", json!([])).await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        Some((
                            o.get("Name")?.as_str()?.to_owned(),
                            o.get("Value")?.as_str().unwrap_or_default().to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn config_categories(cfg: &[(String, String)]) -> Vec<(String, String)> {
        // CategoryN.Name / CategoryN.DestDir
        let mut out = Vec::new();
        for (k, v) in cfg {
            if let Some(n) = k.strip_prefix("Category").and_then(|r| r.strip_suffix(".Name")) {
                let dir_key = format!("Category{n}.DestDir");
                let dir = cfg
                    .iter()
                    .find(|(k, _)| *k == dir_key)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                out.push((v.clone(), dir));
            }
        }
        out
    }

    fn categories_cfg(&self) -> Vec<String> {
        common::configured_categories_all(&[
            &self.settings.movie_category,
            &self.settings.tv_category,
            &self.settings.book_category,
            &self.settings.music_category,
        ])
    }

    fn ours(&self, cat: &str) -> bool {
        self.categories_cfg().iter().any(|c| c.eq_ignore_ascii_case(cat))
    }

    async fn edit(&self, command: &str, param: &str, id: &DownloadId) -> Result<(), ClientError> {
        let nzb_id: i64 = id
            .as_str()
            .parse()
            .map_err(|_| ClientError::NotFound(id.0.clone()))?;
        let ok = self.rpc("editqueue", json!([command, param, [nzb_id]])).await?;
        if ok.as_bool() == Some(false) {
            return Err(ClientError::Api(format!("NZBGet refused {command} for {id}")));
        }
        Ok(())
    }

    async fn run_checks(&self) -> (TestReport, Option<ClientError>) {
        let mut r = TestReport::default();
        let v = match self.version().await {
            Ok(v) => {
                r.pass("connect");
                r.pass("auth");
                v
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
        };
        if !version_at_least(&v, MIN_VERSION) {
            let e = ClientError::UnsupportedVersion {
                found: v,
                minimum: MIN_VERSION.into(),
            };
            r.fail("version", e.to_string());
            return (r, Some(e));
        }
        r.pass("version");
        match self.config().await {
            Ok(cfg) => {
                let have: Vec<String> = Self::config_categories(&cfg).into_iter().map(|c| c.0).collect();
                let missing = crate::sabnzbd::health::missing_categories(&have, &self.categories_cfg());
                if missing.is_empty() {
                    r.pass("categories");
                } else {
                    let m = format!("missing in NZBGet: {}", missing.join(", "));
                    r.fail("categories", m.clone());
                    return (r, Some(ClientError::InvalidSettings(m)));
                }
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
impl DownloadClient for NzbGet {
    fn protocol(&self) -> Protocol {
        Protocol::Usenet
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
        let category = common::category_for(
            opts.category.as_deref(),
            opts.media_kind,
            &self.settings.movie_category,
            &self.settings.tv_category,
        )
        .unwrap_or_default();
        let prio = opts.priority.unwrap_or(if opts.is_recent {
            self.settings.recent_priority
        } else {
            self.settings.older_priority
        });
        let paused = opts.add_paused || self.settings.add_paused || prio == Priority::Paused;
        let title = common::sanitize_file_name(if opts.title.is_empty() {
            "release"
        } else {
            &opts.title
        });
        let content = match release {
            ReleasePayload::NzbFile { bytes, .. } => {
                common::validate_nzb(bytes)?;
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(bytes)
            }
            ReleasePayload::Url(u) => u.clone(),
            other => {
                return Err(ClientError::InvalidPayload(format!(
                    "NZBGet cannot download a {}",
                    other.kind()
                )));
            }
        };
        // append(NZBFilename, Content, Category, Priority, AddToTop, AddPaused, DupeKey, DupeScore, DupeMode, PPParameters)
        let id = self
            .rpc(
                "append",
                json!([
                    format!("{title}.nzb"),
                    content,
                    category,
                    priority_value(prio),
                    prio == Priority::Force,
                    paused,
                    "",
                    0,
                    "SCORE",
                    []
                ]),
            )
            .await?;
        match id.as_i64() {
            Some(n) if n > 0 => Ok(DownloadId(n.to_string())),
            _ => Err(ClientError::Rejected("NZBGet rejected the NZB".into())),
        }
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let (groups, history, status) = tokio::try_join!(
            self.rpc("listgroups", json!([0])),
            self.rpc("history", json!([false])),
            self.rpc("status", json!([])),
        )?;
        let global_paused = status
            .get("DownloadPaused")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let groups: Vec<QueueGroup> = serde_json::from_value(groups)?;
        let history: Vec<HistoryEntry> = serde_json::from_value(history)?;
        let mut out = Vec::new();
        for g in groups.into_iter().filter(|g| self.ours(&g.category)) {
            let mut state = queue_state(&g.status);
            if global_paused && state == DownloadState::Downloading {
                state = DownloadState::Paused;
            }
            let mut item = DownloadItem::new(g.nzb_id.to_string(), g.nzb_name, state);
            item.category = Some(g.category);
            item.total_bytes = size(g.file_size_lo, g.file_size_hi);
            let remaining = size(g.remaining_size_lo, g.remaining_size_hi);
            let paused_size = size(g.paused_size_lo, g.paused_size_hi);
            item.remaining_bytes = remaining;
            if state == DownloadState::Downloading && remaining > 0 && remaining == paused_size {
                item.state = DownloadState::Paused;
            }
            item.priority = Some(priority_from(g.max_priority));
            if item.state == DownloadState::PostProcessing {
                item.message = Some(g.status.to_ascii_lowercase().replace('_', " "));
            }
            out.push(item);
        }
        for h in history.into_iter().filter(|h| self.ours(&h.category)) {
            let (state, failure, message) = history_state(&h);
            if h.status == "DELETED/MANUAL" {
                continue; // removed by the user; not ours to handle
            }
            let mut item = DownloadItem::new(h.nzb_id.to_string(), h.name, state);
            item.category = Some(h.category);
            item.total_bytes = size(h.file_size_lo, h.file_size_hi);
            item.failure = failure;
            item.message = message;
            let dir = if h.final_dir.is_empty() {
                h.dest_dir
            } else {
                h.final_dir
            };
            if state == DownloadState::Completed {
                item.output_path = Some(dir).filter(|d| !d.is_empty());
                item.can_move_files = true;
            }
            item.can_be_removed = matches!(state, DownloadState::Completed | DownloadState::Failed);
            out.push(item);
        }
        Ok(out)
    }

    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError> {
        let nzb_id: i64 = id
            .as_str()
            .parse()
            .map_err(|_| ClientError::NotFound(id.0.clone()))?;
        let groups: Vec<QueueGroup> = serde_json::from_value(self.rpc("listgroups", json!([0])).await?)?;
        if groups.iter().any(|g| g.nzb_id == nzb_id) {
            let cmd = if delete_data {
                "GroupFinalDelete"
            } else {
                "GroupDelete"
            };
            self.edit(cmd, "", id).await
        } else {
            let cmd = if delete_data {
                "HistoryFinalDelete"
            } else {
                "HistoryDelete"
            };
            self.edit(cmd, "", id).await
        }
    }

    async fn test(&self) -> Result<(), ClientError> {
        self.run_checks().await.1.map_or(Ok(()), Err)
    }

    async fn test_report(&self) -> TestReport {
        self.run_checks().await.0
    }

    async fn status(&self) -> Result<ClientStatus, ClientError> {
        let version = self.version().await?;
        let cfg = self.config().await?;
        let get = |k: &str| {
            cfg.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
        };
        let mut warnings = Vec::new();
        if !version_at_least(&version, MIN_VERSION) {
            warnings.push(HealthWarning::new(
                HealthKind::VersionTooOld,
                format!("NZBGet {version} is not supported; please upgrade to {MIN_VERSION} or newer"),
            ));
        }
        if get("KeepHistory") == "0" {
            warnings.push(
                HealthWarning::new(
                    HealthKind::HistoryRetention,
                    "NZBGet KeepHistory is 0, so completed downloads vanish before Reelix can import them",
                )
                .at("/#config/HISTORY"),
            );
        }
        let cats = Self::config_categories(&cfg);
        let have: Vec<String> = cats.iter().map(|c| c.0.clone()).collect();
        for c in crate::sabnzbd::health::missing_categories(&have, &self.categories_cfg()) {
            warnings.push(
                HealthWarning::new(
                    HealthKind::CategoryMissing,
                    format!("Category `{c}` does not exist in NZBGet"),
                )
                .at("/#config/CATEGORIES"),
            );
        }
        let dest = get("DestDir").to_owned();
        let append_cat = get("AppendCategoryDir") != "no";
        let mut output_dirs = vec![OutputDir {
            category: None,
            path: dest.clone(),
        }];
        for c in self.categories_cfg() {
            if let Some((name, dir)) = cats.iter().find(|(n, _)| n.eq_ignore_ascii_case(&c)) {
                let path = if !dir.is_empty() {
                    dir.clone()
                } else if append_cat {
                    format!("{}/{}", dest.trim_end_matches('/'), name)
                } else {
                    dest.clone()
                };
                output_dirs.push(OutputDir {
                    category: Some(c),
                    path,
                });
            }
        }
        output_dirs.retain(|d| !d.path.is_empty());
        Ok(ClientStatus {
            version: Some(version),
            output_dirs,
            warnings,
        })
    }

    async fn categories(&self) -> Result<Vec<String>, ClientError> {
        Ok(Self::config_categories(&self.config().await?)
            .into_iter()
            .map(|c| c.0)
            .collect())
    }

    async fn set_priority(&self, id: &DownloadId, priority: Priority) -> Result<(), ClientError> {
        if priority == Priority::Paused {
            return self.pause(id).await;
        }
        self.edit("GroupSetPriority", &priority_value(priority).to_string(), id)
            .await
    }

    async fn pause(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.edit("GroupPause", "", id).await
    }

    async fn resume(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.edit("GroupResume", "", id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(status: &str, unpack: &str) -> HistoryEntry {
        HistoryEntry {
            nzb_id: 1,
            name: "x".into(),
            category: "movies".into(),
            file_size_lo: 0,
            file_size_hi: 0,
            status: status.into(),
            unpack_status: unpack.into(),
            dest_dir: String::new(),
            final_dir: String::new(),
        }
    }

    #[test]
    fn history_mapping_table() {
        use DownloadState::*;
        use UsenetFailure::*;
        let cases = [
            ("SUCCESS/ALL", "", Completed, None),
            ("SUCCESS/UNPACK", "", Completed, None),
            ("WARNING/SCRIPT", "", Completed, None),
            ("WARNING/SPACE", "", Failed, Some(DiskFull)),
            ("WARNING/PASSWORD", "", Failed, Some(PasswordProtected)),
            ("WARNING/DAMAGED", "", Failed, Some(RepairFailed)),
            ("FAILURE/PAR", "", Failed, Some(RepairFailed)),
            ("FAILURE/UNPACK", "FAILURE", Failed, Some(UnpackFailed)),
            ("FAILURE/UNPACK", "PASSWORD", Failed, Some(PasswordProtected)),
            ("FAILURE/UNPACK", "SPACE", Failed, Some(DiskFull)),
            ("FAILURE/HEALTH", "", Failed, Some(MissingArticles)),
            ("FAILURE/FETCH", "", Failed, Some(FetchFailed)),
            ("DELETED/DUPE", "", Failed, Some(Duplicate)),
            ("DELETED/HEALTH", "", Failed, Some(MissingArticles)),
        ];
        for (s, u, state, f) in cases {
            let (got_state, got_f, _) = history_state(&h(s, u));
            assert_eq!((got_state, got_f), (state, f), "{s}");
        }
    }

    #[test]
    fn queue_mapping_and_sizes() {
        assert_eq!(queue_state("DOWNLOADING"), DownloadState::Downloading);
        assert_eq!(queue_state("UNPACKING"), DownloadState::PostProcessing);
        assert_eq!(queue_state("PAUSED"), DownloadState::Paused);
        assert_eq!(size(5, 1), (1 << 32) + 5);
        assert_eq!(priority_from(900), Priority::Force);
    }
}
