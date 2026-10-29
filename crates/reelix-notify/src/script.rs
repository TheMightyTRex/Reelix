//! Custom script: runs an executable directly (no shell) with event data in `REELIX_*`
//! environment variables.
//!
//! The child gets a cleared environment (only `PATH`, `HOME`, `LANG`, `TZ`, `TMPDIR` and
//! Windows essentials are inherited) so server secrets in the parent's environment never
//! leak. Variables:
//!
//! | Variable | Content |
//! |---|---|
//! | `REELIX_EVENT_TYPE` | [`EventKind`] id (`grab`, `upgrade`, `request_approved`, …) |
//! | `REELIX_EVENT_JSON` | the [webhook payload](crate::webhook) as JSON |
//! | `REELIX_INSTANCE_NAME`, `REELIX_TITLE`, `REELIX_MESSAGE` | rendered text |
//! | `REELIX_MEDIA_KIND`, `_ID`, `_TITLE`, `_YEAR`, `_TMDB_ID`, `_IMDB_ID`, `_TVDB_ID`, `REELIX_EPISODES` | media (prefix `REELIX_MEDIA`) |
//! | `REELIX_RELEASE_TITLE`, `_QUALITY`, `_INDEXER`, `_SIZE`, `_GROUP`, `_DOWNLOAD_CLIENT`, `_DOWNLOAD_ID` | release |
//! | `REELIX_FILE_PATH`, `_QUALITY`, `_SIZE`, `REELIX_IS_UPGRADE`, `REELIX_DELETE_REASON` | file |
//! | `REELIX_REQUEST_ID`, `_STATUS`, `REELIX_REQUESTED_BY`, `REELIX_REQUEST_SEASONS` | request |
//! | `REELIX_ISSUE_ID`, `_TYPE`, `_MESSAGE`, `REELIX_ISSUE_REPORTED_BY` | issue |
//! | `REELIX_HEALTH_SOURCE`, `_LEVEL`, `_MESSAGE` | health |
//! | `REELIX_UPDATE_PREVIOUS_VERSION`, `REELIX_UPDATE_NEW_VERSION` | update |

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::Value;

use crate::common::{Settings, schema as mk_schema, truncate};
use crate::event::{FileSummary, ReleaseSummary};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "script";

const INHERITED: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "SystemRoot",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "PATHEXT",
    "COMSPEC",
];

pub fn schema() -> ProviderSchema {
    mk_schema(
        ID,
        "Custom script",
        "Runs an executable with event data in REELIX_* environment variables",
        None,
        vec![
            FieldSchema::new("path", "Path", FieldKind::Path)
                .required()
                .help("Absolute path to an executable; it is run directly, never through a shell"),
            FieldSchema::new("arguments", "Arguments", FieldKind::List)
                .help("Passed verbatim as separate arguments")
                .advanced(),
        ],
    )
}

/// Custom script notifier.
pub struct Script {
    path: PathBuf,
    arguments: Vec<String>,
    instance: String,
    timeout: Duration,
    events: EventMask,
}

impl Script {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        let path = PathBuf::from(s.str("path")?);
        if !path.is_absolute() {
            return Err(NotifyError::settings("script path must be absolute"));
        }
        Ok(Self {
            path,
            arguments: s.list("arguments"),
            instance: ctx.app_name.clone(),
            timeout: ctx.timeout,
            events: s.events()?,
        })
    }

    /// Environment variables passed to the script for an event.
    pub fn env(&self, event: &NotificationEvent) -> Vec<(String, String)> {
        let mut env = Env(Vec::new());
        let m = Message::from_event(event);
        env.set("EVENT_TYPE", event.kind().as_str());
        env.set(
            "EVENT_JSON",
            crate::webhook::payload(event, &self.instance).to_string(),
        );
        env.set("INSTANCE_NAME", &self.instance);
        env.set("TITLE", &m.title);
        env.set("MESSAGE", m.text());
        if let Some(md) = event.media() {
            env.set(
                "MEDIA_KIND",
                match md.kind {
                    MediaKind::Movie => "movie",
                    MediaKind::Series => "series",
                },
            );
            env.opt("MEDIA_ID", md.id);
            env.set("MEDIA_TITLE", &md.title);
            env.opt("MEDIA_YEAR", md.year);
            env.opt("MEDIA_TMDB_ID", md.tmdb_id);
            env.opt("MEDIA_IMDB_ID", md.imdb_id.as_ref());
            env.opt("MEDIA_TVDB_ID", md.tvdb_id);
            if !md.episodes.is_empty() {
                let eps: Vec<String> = md.episodes.iter().map(|e| e.label()).collect();
                env.set("EPISODES", eps.join(","));
            }
        }
        match event {
            NotificationEvent::Grab { release, .. } => env.release(release),
            NotificationEvent::Import {
                release,
                file,
                is_upgrade,
                ..
            } => {
                if let Some(r) = release {
                    env.release(r);
                }
                env.file(file);
                env.set("IS_UPGRADE", if *is_upgrade { "true" } else { "false" });
            }
            NotificationEvent::Rename { files, .. } => {
                let p: Vec<&str> = files.iter().map(|f| f.previous_path.as_str()).collect();
                let n: Vec<&str> = files.iter().map(|f| f.new_path.as_str()).collect();
                env.set("RENAME_PREVIOUS_PATHS", p.join("|"));
                env.set("RENAME_NEW_PATHS", n.join("|"));
            }
            NotificationEvent::MediaDeleted { files_deleted, .. } => {
                env.set("FILES_DELETED", if *files_deleted { "true" } else { "false" });
            }
            NotificationEvent::FileDeleted { file, reason, .. } => {
                env.file(file);
                env.set(
                    "DELETE_REASON",
                    serde_json::to_value(reason)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default(),
                );
            }
            NotificationEvent::HealthIssue(h) | NotificationEvent::HealthRestored(h) => {
                env.set("HEALTH_SOURCE", &h.source);
                env.set("HEALTH_LEVEL", format!("{:?}", h.level).to_lowercase());
                env.set("HEALTH_MESSAGE", &h.message);
            }
            NotificationEvent::ManualInteractionRequired { release, message, .. } => {
                env.release(release);
                env.set("MANUAL_INTERACTION_MESSAGE", message);
            }
            NotificationEvent::RequestPending { request, .. }
            | NotificationEvent::RequestApproved { request, .. }
            | NotificationEvent::RequestDeclined { request, .. }
            | NotificationEvent::RequestAvailable { request, .. } => {
                env.set("REQUEST_ID", request.id.to_string());
                env.set("REQUEST_STATUS", format!("{:?}", request.status).to_lowercase());
                env.set("REQUESTED_BY", &request.requested_by);
                let s: Vec<String> = request.seasons.iter().map(u32::to_string).collect();
                env.set("REQUEST_SEASONS", s.join(","));
                env.set("REQUEST_IS_4K", if request.is_4k { "true" } else { "false" });
                env.opt("REQUEST_COMMENT", request.comment.as_ref());
            }
            NotificationEvent::IssueReported { issue, .. }
            | NotificationEvent::IssueResolved { issue, .. } => {
                env.set("ISSUE_ID", issue.id.to_string());
                env.set("ISSUE_TYPE", issue.issue_type.to_string().to_lowercase());
                env.set("ISSUE_MESSAGE", &issue.message);
                env.set("ISSUE_REPORTED_BY", &issue.reported_by);
                env.opt("ISSUE_RESOLVED_BY", issue.resolved_by.as_ref());
            }
            NotificationEvent::ApplicationUpdate(u) => {
                env.set("UPDATE_PREVIOUS_VERSION", &u.previous_version);
                env.set("UPDATE_NEW_VERSION", &u.new_version);
            }
            NotificationEvent::Test { .. } | NotificationEvent::MediaAdded { .. } => {}
        }
        env.0
    }
}

struct Env(Vec<(String, String)>);

impl Env {
    fn set(&mut self, key: &str, value: impl AsRef<str>) {
        // NUL bytes would make spawning fail; they never carry meaning here.
        let v = value.as_ref().replace('\0', "");
        self.0.push((format!("REELIX_{key}"), v));
    }

    fn opt(&mut self, key: &str, value: Option<impl ToString>) {
        if let Some(v) = value {
            self.set(key, v.to_string());
        }
    }

    fn release(&mut self, r: &ReleaseSummary) {
        self.set("RELEASE_TITLE", &r.title);
        self.opt("RELEASE_QUALITY", r.quality.as_ref());
        self.opt("RELEASE_INDEXER", r.indexer.as_ref());
        self.opt("RELEASE_SIZE", r.size_bytes);
        self.opt("RELEASE_GROUP", r.release_group.as_ref());
        self.opt("RELEASE_DOWNLOAD_CLIENT", r.download_client.as_ref());
        self.opt("RELEASE_DOWNLOAD_ID", r.download_id.as_ref());
    }

    fn file(&mut self, f: &FileSummary) {
        self.set("FILE_PATH", &f.path);
        self.opt("FILE_QUALITY", f.quality.as_ref());
        self.opt("FILE_SIZE", f.size_bytes);
    }
}

#[async_trait]
impl Notifier for Script {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.args(&self.arguments)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for key in INHERITED {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        cmd.envs(self.env(event));
        let child = cmd
            .spawn()
            .map_err(|e| NotifyError::Script(format!("failed to start {}: {e}", self.path.display())))?;
        let out = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| NotifyError::Script(format!("timed out after {:?}", self.timeout)))?
            .map_err(|e| NotifyError::Script(e.to_string()))?;
        if !out.status.success() {
            let mut stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            truncate(&mut stderr, 500);
            return Err(NotifyError::Script(format!(
                "exited with {}: {stderr}",
                out.status
            )));
        }
        tracing::debug!(
            script = %self.path.display(),
            event = event.kind().as_str(),
            "custom script finished"
        );
        Ok(())
    }
}
