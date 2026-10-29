//! SABnzbd adapter (README "Download Client Integrations → SABnzbd").
//!
//! Supports SABnzbd 3.0+. Only Reelix's categories are read, so other jobs are never touched.

mod api;
pub mod health;
pub mod models;

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use reelix_core::{ContentKind, Protocol};
use serde::Deserialize;

use crate::common::{self, CommonSettings, HttpSettings, de_priority};
use crate::model::*;
use crate::{BuildContext, ClientError, DownloadClient};

pub const IMPLEMENTATION: &str = "sabnzbd";

/// How the NZB reaches SABnzbd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrabMethod {
    /// Reelix fetches the NZB (through the indexer) and uploads it.
    UploadNzb,
    /// SABnzbd fetches the URL itself.
    SendUrl,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SabnzbdSettings {
    #[serde(flatten)]
    pub common: CommonSettings,
    #[serde(flatten)]
    pub http: HttpSettings,
    pub api_key: String,
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
    #[serde(deserialize_with = "de_priority")]
    pub recent_priority: Priority,
    #[serde(deserialize_with = "de_priority")]
    pub older_priority: Priority,
    pub grab_method: GrabMethod,
}

impl SabnzbdSettings {
    pub fn category_for(&self, opts: &AddOptions) -> Option<String> {
        common::category_for(
            opts.category.as_deref(),
            opts.media_kind,
            &self.movie_category,
            &self.tv_category,
        )
    }

    /// Explicit priority, else recent/older; `add_paused` forces Paused.
    pub fn priority_for(&self, opts: &AddOptions) -> Priority {
        if opts.add_paused {
            return Priority::Paused;
        }
        opts.priority.unwrap_or(if opts.is_recent {
            self.recent_priority
        } else {
            self.older_priority
        })
    }

    pub fn categories(&self) -> Vec<String> {
        common::configured_categories_all(&[
            &self.movie_category,
            &self.tv_category,
            &self.book_category,
            &self.music_category,
        ])
    }
}

/// Settings form (README "Settings Schema").
pub fn schema() -> ProviderSchema {
    let mut fields = vec![common::f_name("SABnzbd"), common::f_host(), common::f_port(8080)];
    fields.extend(common::f_ssl());
    fields.push(common::f_url_base("/sabnzbd"));
    fields.push(
        FieldSchema::new("api_key", "API key", FieldKind::Secret)
            .required()
            .help("The full API key (Config → General). The NZB key can only add jobs and is rejected."),
    );
    fields.push(
        common::f_username(None)
            .help("Only if SABnzbd sits behind HTTP basic auth")
            .advanced(),
    );
    fields.push(
        common::f_password(None)
            .help("Only if SABnzbd sits behind HTTP basic auth")
            .advanced(),
    );
    fields.extend(common::f_categories("movies", "tv", "category"));
    fields.extend(common::f_extra_categories("category"));
    fields.extend(common::f_recent_older_priority());
    fields.push(
        FieldSchema::new(
            "grab_method",
            "Grab method",
            FieldKind::Select {
                options: vec![
                    SelectOption {
                        value: "upload_nzb".into(),
                        label: "Upload NZB (Reelix fetches it)".into(),
                    },
                    SelectOption {
                        value: "send_url".into(),
                        label: "Send URL (SABnzbd fetches it)".into(),
                    },
                ],
            },
        )
        .default_value("upload_nzb")
        .help("Uploading keeps the indexer's API key away from SABnzbd and validates the NZB first")
        .advanced(),
    );
    fields.extend(common::f_common_tail());
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "SABnzbd".into(),
        description: Some("Usenet downloader (version 3.0 or newer)".into()),
        info_url: Some("https://sabnzbd.org/wiki/configuration/4.3/api".into()),
        fields,
    }
}

/// SABnzbd client.
pub struct Sabnzbd {
    http: reqwest::Client,
    base_url: String,
    settings: SabnzbdSettings,
    info: ClientInfo,
}

impl Sabnzbd {
    pub fn from_settings(settings: &serde_json::Value, ctx: &BuildContext) -> Result<Self, ClientError> {
        let s: SabnzbdSettings = common::decode_settings(&schema(), settings)?;
        Self::new(s, ctx)
    }

    pub fn new(settings: SabnzbdSettings, ctx: &BuildContext) -> Result<Self, ClientError> {
        if settings.api_key.trim().is_empty() {
            return Err(ClientError::InvalidSettings("API key is required".into()));
        }
        Ok(Self {
            http: settings.http.client(ctx)?,
            base_url: settings.http.base_url()?,
            info: settings.common.info(IMPLEMENTATION),
            settings,
        })
    }

    pub fn settings(&self) -> &SabnzbdSettings {
        &self.settings
    }

    async fn fetch_nzb(&self, url: &str) -> Result<Vec<u8>, ClientError> {
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(ClientError::InvalidPayload(format!(
                "fetching NZB failed: HTTP {}",
                resp.status()
            )));
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Runs connect → version → auth → categories; returns the report and the first error.
    async fn run_checks(&self) -> (TestReport, Option<ClientError>) {
        let mut r = TestReport::default();
        let version = match self.version().await {
            Ok(v) => {
                r.pass("connect");
                v
            }
            Err(e) => {
                r.fail("connect", e.to_string());
                return (r, Some(e));
            }
        };
        if let Err(e) = self.get_config(Some("misc")).await {
            let e = match e {
                ClientError::Auth(_) if self.is_nzb_key().await => {
                    ClientError::Auth("this is the NZB key; Reelix needs the full API key".into())
                }
                ClientError::Auth(m) => ClientError::Auth(format!("API key rejected ({m})")),
                other => other,
            };
            r.fail("auth", e.to_string());
            return (r, Some(e));
        }
        r.pass("auth");
        if health::version_supported(&version) {
            r.pass("version");
        } else {
            let e = ClientError::UnsupportedVersion {
                found: version,
                minimum: health::MIN_VERSION.into(),
            };
            r.fail("version", e.to_string());
            return (r, Some(e));
        }
        match self.get_cats().await {
            Ok(cats) => {
                let missing = health::missing_categories(&cats, &self.settings.categories());
                if missing.is_empty() {
                    r.pass("categories");
                } else {
                    let msg = format!("missing in SABnzbd: {}", missing.join(", "));
                    r.fail("categories", msg.clone());
                    return (r, Some(ClientError::InvalidSettings(msg)));
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
impl DownloadClient for Sabnzbd {
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

    fn payload_preference(&self) -> PayloadPreference {
        match self.settings.grab_method {
            GrabMethod::UploadNzb => PayloadPreference::File,
            GrabMethod::SendUrl => PayloadPreference::Url,
        }
    }

    async fn add(&self, release: &ReleasePayload, opts: &AddOptions) -> Result<DownloadId, ClientError> {
        let category = self.settings.category_for(opts);
        let priority = models::priority_value(self.settings.priority_for(opts));
        let title = opts.title.trim();
        let file_name = format!(
            "{}.nzb",
            common::sanitize_file_name(if title.is_empty() { "release" } else { title })
        );
        let resp = match release {
            ReleasePayload::NzbFile { bytes, name } => {
                common::validate_nzb(bytes)?;
                let name = if name.is_empty() { &file_name } else { name };
                self.add_file(bytes.clone(), name, title, category.as_deref(), priority)
                    .await?
            }
            ReleasePayload::Url(url) => match self.settings.grab_method {
                GrabMethod::SendUrl => self.add_url(url, title, category.as_deref(), priority).await?,
                GrabMethod::UploadNzb => {
                    let bytes = self.fetch_nzb(url).await?;
                    common::validate_nzb(&bytes)?;
                    self.add_file(bytes, &file_name, title, category.as_deref(), priority)
                        .await?
                }
            },
            other => {
                return Err(ClientError::InvalidPayload(format!(
                    "SABnzbd cannot download a {}",
                    other.kind()
                )));
            }
        };
        if !resp.status {
            return Err(ClientError::Rejected("SABnzbd did not accept the NZB".into()));
        }
        resp.nzo_ids
            .into_iter()
            .next()
            .map(DownloadId::from)
            .ok_or_else(|| ClientError::Rejected("SABnzbd returned no nzo_id".into()))
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let mut out: Vec<DownloadItem> = Vec::new();
        for cat in self.settings.categories() {
            let (queue, history) = tokio::try_join!(self.queue(Some(&cat), None), self.history(Some(&cat)))?;
            let paused = queue.paused;
            let hist: Vec<DownloadItem> = history.slots.into_iter().map(|h| h.into_item()).collect();
            for q in queue.slots {
                // A job can briefly appear in both lists; history is authoritative.
                if !hist.iter().any(|h| h.download_id.0 == q.nzo_id) {
                    out.push(q.into_item(paused));
                }
            }
            out.extend(hist);
        }
        Ok(out)
    }

    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError> {
        let in_queue = self
            .queue(None, Some(id.as_str()))
            .await?
            .slots
            .iter()
            .any(|s| s.nzo_id == id.0);
        self.delete(in_queue, id.as_str(), delete_data).await
    }

    async fn test(&self) -> Result<(), ClientError> {
        match self.run_checks().await {
            (_, Some(e)) => Err(e),
            (_, None) => Ok(()),
        }
    }

    async fn test_report(&self) -> TestReport {
        self.run_checks().await.0
    }

    async fn status(&self) -> Result<ClientStatus, ClientError> {
        let version = self.version().await?;
        let config = match self.get_config(None).await {
            Ok(c) => c,
            Err(ClientError::Auth(m)) => {
                let w = if self.is_nzb_key().await {
                    HealthWarning::new(
                        HealthKind::NzbKeyOnly,
                        "SABnzbd is configured with the NZB key; Reelix needs the full API key",
                    )
                } else {
                    HealthWarning::new(
                        HealthKind::AuthFailed,
                        format!("SABnzbd rejected the API key: {m}"),
                    )
                };
                return Ok(ClientStatus {
                    version: Some(version),
                    output_dirs: Vec::new(),
                    warnings: vec![w.at("/config/general/")],
                });
            }
            Err(e) => return Err(e),
        };
        let cats = self.settings.categories();
        let mut warnings = health::evaluate(&config, Some(&version), &cats);
        for text in self.warnings().await.unwrap_or_default() {
            warnings.push(
                HealthWarning::new(HealthKind::ClientWarning, format!("SABnzbd: {text}")).at("/status/"),
            );
        }
        Ok(ClientStatus {
            version: Some(version),
            output_dirs: health::output_dirs(&config, &cats),
            warnings,
        })
    }

    async fn categories(&self) -> Result<Vec<String>, ClientError> {
        Ok(self.get_cats().await?.into_iter().filter(|c| c != "*").collect())
    }

    async fn set_priority(&self, id: &DownloadId, priority: Priority) -> Result<(), ClientError> {
        let v = models::priority_value(priority).to_string();
        self.queue_action("priority", id.as_str(), Some(&v)).await
    }

    async fn pause(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.queue_action("pause", id.as_str(), None).await
    }

    async fn resume(&self, id: &DownloadId) -> Result<(), ClientError> {
        self.queue_action("resume", id.as_str(), None).await
    }
}
