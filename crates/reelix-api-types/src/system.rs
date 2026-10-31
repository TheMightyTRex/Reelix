//! System maintenance DTOs: backups, logs, updates, server settings and per-user UI settings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------- Backups ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum BackupKind {
    /// Created with "Back up now" or through the API.
    Manual,
    /// Created by the scheduled backup task; subject to retention.
    Scheduled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupDto {
    /// File name inside the backup folder, e.g. `reelix_backup_manual_2026.10.30_21.04.11.zip`.
    pub name: String,
    pub kind: BackupKind,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
    /// Reelix version that wrote the backup, when the archive records it.
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupSettings {
    /// Days between scheduled backups; 0 turns them off.
    pub interval_days: u32,
    /// Scheduled backups to keep (manual backups are never pruned).
    pub retention: u32,
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            interval_days: 7,
            retention: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PendingRestoreDto {
    /// Backup the restore was staged from (or `upload`).
    pub source: String,
    pub staged_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupsOverview {
    /// Newest first.
    pub backups: Vec<BackupDto>,
    pub settings: BackupSettings,
    /// Folder the backups are written to.
    pub backup_dir: String,
    /// A restore waiting for the next restart.
    pub pending_restore: Option<PendingRestoreDto>,
    /// False when the secret key comes from `REELIX_SECRET_KEY`, so backups don't contain it.
    pub includes_secret_key: bool,
}

// ---------- Logs ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Self::Trace,
            "debug" => Self::Debug,
            "info" => Self::Info,
            "warn" | "warning" => Self::Warn,
            "error" => Self::Error,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LogFileDto {
    pub name: String,
    pub size_bytes: u64,
    pub modified: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LogEntryDto {
    pub time: Option<DateTime<Utc>>,
    pub level: Option<LogLevel>,
    /// Module that logged the entry, when the format records it separately.
    pub target: Option<String>,
    /// Message plus fields; may span several lines.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LogsDto {
    /// File the entries come from; `None` when there are no log files yet.
    pub file: Option<String>,
    /// Newest first.
    pub entries: Vec<LogEntryDto>,
    /// Only the end of a large file was read.
    pub truncated: bool,
}

// ---------- Updates ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseAssetDto {
    pub name: String,
    pub url: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseDto {
    /// Version from the tag, without a leading `v`.
    pub version: String,
    pub name: String,
    /// Release notes (Markdown).
    pub notes: String,
    pub url: String,
    pub published_at: Option<DateTime<Utc>>,
    pub prerelease: bool,
    /// Newer than the running version.
    pub newer: bool,
    pub assets: Vec<ReleaseAssetDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum InstallKind {
    Docker,
    Windows,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateStatusDto {
    pub current_version: String,
    /// GitHub repository the releases come from, e.g. `mvanhare/Reelix`.
    pub repository: String,
    pub install: InstallKind,
    /// Newest published release, if any.
    pub latest: Option<ReleaseDto>,
    pub update_available: bool,
    /// Recent releases, newest first.
    pub releases: Vec<ReleaseDto>,
    pub checked_at: Option<DateTime<Utc>>,
    /// Why the last check failed.
    pub error: Option<String>,
    /// Check for updates on a schedule (and show a health warning when one is available).
    pub automatic: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateSettings {
    pub automatic: bool,
}

// ---------- Server settings (config.toml) ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ServerSettingsValues {
    pub bind: String,
    pub port: u16,
    /// `""` or a path like `/reelix`.
    pub url_base: String,
    /// `trace`, `debug`, `info`, `warn`, `error` or a tracing filter directive.
    pub log_level: String,
    /// `pretty` or `json`.
    pub log_format: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ServerSettingsDto {
    /// Values saved in `config.toml` (defaults where unset).
    pub saved: ServerSettingsValues,
    /// Values the running server uses.
    pub running: ServerSettingsValues,
    pub config_file: String,
    /// Settings forced by environment variables or command-line flags, e.g. `port (REELIX_PORT)`.
    pub overridden: Vec<String>,
    /// Saved values differ from the running ones; restart Reelix to apply them.
    pub restart_required: bool,
}

// ---------- UI settings (per user) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum DateFormat {
    /// 2026-10-28
    #[default]
    Iso,
    /// 10/28/2026
    Us,
    /// 28/10/2026
    Eu,
    /// Oct 28, 2026
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum TimeFormat {
    /// 14:03
    #[default]
    H24,
    /// 2:03 PM
    H12,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum FirstDayOfWeek {
    #[default]
    Monday,
    Sunday,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum CalendarView {
    #[default]
    Month,
    Week,
    Agenda,
}

impl CalendarView {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Month => "month",
            Self::Week => "week",
            Self::Agenda => "agenda",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum LibraryView {
    #[default]
    Posters,
    Overview,
    Table,
}

impl LibraryView {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Posters => "posters",
            Self::Overview => "overview",
            Self::Table => "table",
        }
    }
}

/// Display preferences of the signed-in user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct UiSettings {
    pub date_format: DateFormat,
    pub time_format: TimeFormat,
    /// Show "3h ago" instead of the date where space is short.
    pub relative_dates: bool,
    pub first_day_of_week: FirstDayOfWeek,
    /// Calendar view used until the user picks another one in this browser.
    pub calendar_view: CalendarView,
    /// Library layout used until the user picks another one in this browser.
    pub library_view: LibraryView,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            date_format: DateFormat::default(),
            time_format: TimeFormat::default(),
            relative_dates: true,
            first_day_of_week: FirstDayOfWeek::default(),
            calendar_view: CalendarView::default(),
            library_view: LibraryView::default(),
        }
    }
}
