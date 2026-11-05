//! slskd (Soulseek) adapter: REST API at `{base}/api/v0`, `X-API-Key` auth. Based on slskd 0.26.
//!
//! A grab enqueues every file of one peer folder (`POST /transfers/downloads/{username}`); the
//! download id is `{username}/{remote directory}` and status comes from grouping
//! `GET /transfers/downloads` the same way. slskd moves finished files to
//! `<downloads>/<last remote folder name>/` (its default `${SOURCE_DIRECTORY}` layout), which is
//! the output path reported here. There are no categories: slskd downloads Reelix did not grab
//! are never adopted by the pipeline.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime};
use reelix_core::Protocol;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::soulseek::{self, SoulseekRelease};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::common::{self, CommonSettings, HttpSettings, ok_text};
use crate::model::*;
use crate::{BuildContext, ClientError, DownloadClient};

pub const IMPLEMENTATION: &str = "slskd";

/// Enqueueing waits for the peer to acknowledge every file (slskd allows minutes per file).
const ENQUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug, Clone, Deserialize)]
pub struct SlskdSettings {
    #[serde(flatten)]
    pub common: CommonSettings,
    #[serde(flatten)]
    pub http: HttpSettings,
    pub api_key: String,
    /// slskd's downloads directory as slskd sees it (`directories.downloads`).
    pub downloads_dir: String,
    /// Fail the whole folder when any file fails for good (otherwise import what arrived).
    #[serde(default = "yes")]
    pub fail_on_file_error: bool,
}

fn yes() -> bool {
    true
}

pub fn schema() -> ProviderSchema {
    let mut fields = vec![common::f_name("slskd"), common::f_host(), common::f_port(5030)];
    fields.extend(common::f_ssl());
    fields.push(common::f_url_base(""));
    fields.push(
        FieldSchema::new("api_key", "API key", FieldKind::Secret)
            .required()
            .help("An slskd API key (web.authentication.api_keys or SLSKD_API_KEY)"),
    );
    fields.push(
        FieldSchema::new("downloads_dir", "Downloads folder", FieldKind::Text)
            .required()
            .placeholder("/app/downloads")
            .help("slskd's downloads directory as slskd sees it; add a remote path mapping if Reelix sees it elsewhere"),
    );
    fields.push(
        FieldSchema::new("fail_on_file_error", "Fail if any file fails", FieldKind::Bool)
            .default_value(true)
            .help("Off: import the files that arrived when some fail for good (peer offline, rejected)"),
    );
    fields.push(
        FieldSchema::new("add_indexer", "Also add the slskd indexer", FieldKind::Bool)
            .default_value(true)
            .help("When saving a new client: add an slskd indexer with the same URL and API key"),
    );
    fields.extend(common::f_common_tail());
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "slskd (Soulseek)".into(),
        description: Some("Soulseek downloads through a self-hosted slskd daemon".into()),
        info_url: Some("https://github.com/slskd/slskd".into()),
        fields,
    }
}

pub struct Slskd {
    settings: SlskdSettings,
    info: ClientInfo,
    base: String,
    http: reqwest::Client,
}

impl Slskd {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ClientError> {
        let settings: SlskdSettings = common::decode_settings(&schema(), settings)?;
        if settings.downloads_dir.trim().is_empty() {
            return Err(ClientError::InvalidSettings("downloads_dir is required".into()));
        }
        let base = settings.http.base_url()?;
        Ok(Self {
            info: settings.common.info(IMPLEMENTATION),
            http: settings.http.client(ctx)?,
            base,
            settings,
        })
    }

    /// `{base}/api/v0/{segments…}`, each segment percent-encoded (usernames are free text).
    fn url(&self, segments: &[&str]) -> Result<url::Url, ClientError> {
        let mut u = url::Url::parse(&format!("{}/api/v0", self.base))
            .map_err(|e| ClientError::InvalidSettings(e.to_string()))?;
        u.path_segments_mut()
            .map_err(|()| ClientError::InvalidSettings("URL cannot be a base".into()))?
            .extend(segments);
        Ok(u)
    }

    fn req(&self, method: reqwest::Method, u: url::Url) -> reqwest::RequestBuilder {
        self.http
            .request(method, u)
            .header("X-API-Key", &self.settings.api_key)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, segments: &[&str]) -> Result<T, ClientError> {
        let resp = self.req(reqwest::Method::GET, self.url(segments)?).send().await?;
        Ok(serde_json::from_str(&ok_text(resp).await?)?)
    }

    /// Local path of a folder: `<downloads_dir>/<last remote component>`.
    pub fn output_path(&self, directory: &str) -> String {
        let root = self.settings.downloads_dir.trim();
        let sep = if root.contains('\\') && !root.contains('/') {
            '\\'
        } else {
            '/'
        };
        let leaf = common::sanitize_file_name(soulseek::last_component(directory));
        format!("{}{sep}{leaf}", root.trim_end_matches(['/', '\\']))
    }

    /// Folds one folder's transfers into a [`DownloadItem`].
    pub fn item(&self, username: &str, directory: &str, files: &[Transfer]) -> DownloadItem {
        let mut item = DownloadItem::new(
            soulseek::download_id(username, directory),
            soulseek::last_component(directory),
            DownloadState::Queued,
        );
        let mut done = 0;
        let (mut ok, mut failed, mut active, mut speed) = (0, Vec::new(), 0, 0.0);
        let mut queue_place = None;
        for t in files {
            item.total_bytes += t.size;
            let got = match t.kind() {
                Kind::Succeeded => t.size,
                _ => t.bytes_transferred.min(t.size),
            };
            item.remaining_bytes += t.size - got;
            match t.kind() {
                Kind::Succeeded => {
                    ok += 1;
                    done += 1;
                }
                // slskd retries some failures on its own (`nextAttemptAt`): not final yet.
                Kind::Failed if t.next_attempt_at.is_none() => {
                    done += 1;
                    failed.push(t);
                }
                Kind::Failed | Kind::Queued => {
                    if let Some(p) = t.place_in_queue {
                        queue_place = Some(queue_place.map_or(p, |q: u64| q.min(p)));
                    }
                }
                Kind::Active => {
                    active += 1;
                    speed += t.average_speed.max(0.0);
                }
            }
        }
        let n = files.len();
        let failure_text = || {
            let first = failed
                .iter()
                .find_map(|t| t.exception.clone())
                .unwrap_or_else(|| failed.first().map(|t| t.state.clone()).unwrap_or_default());
            format!("{} of {n} files failed: {first}", failed.len())
        };
        // A file that failed for good fails the folder at once (setting on), or only when
        // nothing at all arrived (setting off).
        let failed_now = !failed.is_empty() && (self.settings.fail_on_file_error || (done == n && ok == 0));
        item.state = if failed_now {
            item.message = Some(failure_text());
            DownloadState::Failed
        } else if done == n {
            if !failed.is_empty() {
                item.message = Some(failure_text());
            }
            DownloadState::Completed
        } else if active > 0 || ok > 0 {
            DownloadState::Downloading
        } else {
            if let Some(p) = queue_place {
                item.message = Some(format!("Queued by the peer (position {p})"));
            }
            DownloadState::Queued
        };
        if active > 0 && speed > 0.0 {
            item.eta_secs = Some((item.remaining_bytes as f64 / speed).ceil() as u64);
        }
        if item.state == DownloadState::Completed {
            item.remaining_bytes = 0;
            item.output_path = Some(self.output_path(directory));
            item.completed_at = files.iter().filter_map(|t| t.ended_at()).max();
        }
        // No seeding on Soulseek: files may be moved and the transfers cleared once imported.
        item.can_move_files = true;
        item.can_be_removed = matches!(item.state, DownloadState::Completed | DownloadState::Failed);
        item
    }

    async fn folder(&self, id: &DownloadId) -> Result<Option<(String, String, Vec<Transfer>)>, ClientError> {
        let (user, dir) =
            soulseek::parse_download_id(id.as_str()).ok_or_else(|| ClientError::NotFound(id.to_string()))?;
        let resp = self
            .req(reqwest::Method::GET, self.url(&["transfers", "downloads", user])?)
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let u: UserTransfers = serde_json::from_str(&ok_text(resp).await?)?;
        Ok(u.directories
            .into_iter()
            .find(|d| d.directory == dir)
            .map(|d| (user.to_owned(), dir.to_owned(), d.files)))
    }
}

/// `GET /transfers/downloads` entry.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserTransfers {
    pub username: String,
    #[serde(default)]
    pub directories: Vec<DirectoryTransfers>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryTransfers {
    pub directory: String,
    #[serde(default)]
    pub files: Vec<Transfer>,
}

/// One file transfer. Absent optional properties are omitted by slskd, not `null`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    /// Flags joined by `", "`: `Queued, Remotely`, `InProgress`, `Completed, Succeeded`, …
    pub state: String,
    #[serde(default)]
    pub bytes_transferred: u64,
    #[serde(default)]
    pub average_speed: f64,
    pub place_in_queue: Option<u64>,
    pub exception: Option<String>,
    pub ended_at: Option<String>,
    pub next_attempt_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Queued,
    Active,
    Succeeded,
    Failed,
}

impl Transfer {
    fn has(&self, flag: &str) -> bool {
        self.state.split(',').any(|s| s.trim() == flag)
    }

    fn kind(&self) -> Kind {
        if self.has("Completed") {
            if self.has("Succeeded") {
                Kind::Succeeded
            } else {
                Kind::Failed
            }
        } else if self.has("InProgress") || self.has("Initializing") {
            Kind::Active
        } else {
            Kind::Queued
        }
    }

    /// Unix seconds; slskd writes some timestamps without a `Z` (they are UTC too).
    fn ended_at(&self) -> Option<i64> {
        let s = self.ended_at.as_deref()?.trim();
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.timestamp())
            .ok()
            .or_else(|| {
                NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                    .ok()
                    .map(|d| d.and_utc().timestamp())
            })
    }
}

#[async_trait]
impl DownloadClient for Slskd {
    fn protocol(&self) -> Protocol {
        Protocol::Soulseek
    }

    fn info(&self) -> &ClientInfo {
        &self.info
    }

    async fn add(&self, release: &ReleasePayload, _opts: &AddOptions) -> Result<DownloadId, ClientError> {
        let ReleasePayload::Soulseek(rel) = release else {
            return Err(ClientError::InvalidPayload(format!(
                "slskd only downloads Soulseek releases, not a {}",
                release.kind()
            )));
        };
        let rel: &SoulseekRelease = rel;
        if rel.files.is_empty() {
            return Err(ClientError::InvalidPayload(
                "the Soulseek release has no files".into(),
            ));
        }
        let body: Vec<Value> = rel
            .files
            .iter()
            .map(|f| json!({"filename": f.filename, "size": f.size}))
            .collect();
        let resp = self
            .req(
                reqwest::Method::POST,
                self.url(&["transfers", "downloads", &rel.username])?,
            )
            .timeout(ENQUEUE_TIMEOUT)
            .json(&body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let text = common::read_text(resp).await?;
        match status {
            401 | 403 => return Err(ClientError::Auth(format!("HTTP {status}"))),
            // Offline / unknown peer, duplicate files… slskd answers with a plain message.
            400 | 404 | 409 | 500 => return Err(ClientError::Rejected(common::snippet(&text))),
            s if !(200..300).contains(&s) => {
                return Err(ClientError::Http {
                    status: s,
                    body: common::snippet(&text),
                });
            }
            _ => {}
        }
        // `{"enqueued": [...], "failed": [...]}`; nothing enqueued means nothing will arrive.
        if let Ok(v) = serde_json::from_str::<Value>(&text)
            && v.get("enqueued")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        {
            return Err(ClientError::Rejected(format!(
                "{} did not accept any of the {} files",
                rel.username,
                rel.files.len()
            )));
        }
        Ok(DownloadId(rel.download_id()))
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let users: Vec<UserTransfers> = self.get_json(&["transfers", "downloads"]).await?;
        Ok(users
            .iter()
            .flat_map(|u| {
                u.directories
                    .iter()
                    .filter(|d| !d.files.is_empty())
                    .map(|d| self.item(&u.username, &d.directory, &d.files))
            })
            .collect())
    }

    /// Cancels and clears the folder's transfers (`DELETE …/{id}?remove=true`). slskd's API
    /// leaves files on disk, so `delete_data` cannot remove them.
    async fn remove(&self, id: &DownloadId, _delete_data: bool) -> Result<(), ClientError> {
        let Some((user, _, files)) = self.folder(id).await? else {
            return Ok(());
        };
        for t in files {
            let mut u = self.url(&["transfers", "downloads", &user, &t.id])?;
            u.query_pairs_mut().append_pair("remove", "true");
            let resp = self.req(reqwest::Method::DELETE, u).send().await?;
            if resp.status().as_u16() != 404 {
                ok_text(resp).await?;
            }
        }
        Ok(())
    }

    async fn test(&self) -> Result<(), ClientError> {
        let r = self.test_report().await;
        match r.first_failure() {
            None => Ok(()),
            Some(msg) => Err(ClientError::Api(msg)),
        }
    }

    async fn test_report(&self) -> TestReport {
        let mut r = TestReport::default();
        let session = match self.url(&["session"]) {
            Ok(u) => self.req(reqwest::Method::GET, u).send().await,
            Err(e) => {
                r.fail("connection", e.to_string());
                return r;
            }
        };
        match session {
            Err(e) => {
                r.fail("connection", ClientError::from(e).to_string());
                return r;
            }
            Ok(resp) => {
                r.pass("connection");
                match ok_text(resp).await {
                    Ok(_) => r.pass("api key"),
                    Err(e) => {
                        r.fail("api key", e.to_string());
                        return r;
                    }
                }
            }
        }
        match self.get_json::<Value>(&["application"]).await {
            Ok(app) => {
                if app.pointer("/server/isLoggedIn").and_then(Value::as_bool) == Some(false) {
                    r.fail("soulseek", "slskd is not logged in to the Soulseek network");
                } else {
                    r.pass("soulseek");
                }
            }
            Err(e) => r.fail("soulseek", e.to_string()),
        }
        r
    }

    async fn status(&self) -> Result<ClientStatus, ClientError> {
        let app: Value = self.get_json(&["application"]).await?;
        let mut s = ClientStatus {
            version: app
                .pointer("/version/current")
                .and_then(Value::as_str)
                .map(str::to_owned),
            output_dirs: vec![OutputDir {
                category: None,
                path: self.settings.downloads_dir.trim().to_owned(),
            }],
            warnings: Vec::new(),
        };
        if app.pointer("/server/isLoggedIn").and_then(Value::as_bool) == Some(false) {
            s.warnings.push(HealthWarning::new(
                HealthKind::ClientWarning,
                "slskd is not logged in to the Soulseek network",
            ));
        }
        Ok(s)
    }
}
