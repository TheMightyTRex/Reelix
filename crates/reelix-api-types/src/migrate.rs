//! Migration from an existing *arr stack (Radarr, Sonarr, Prowlarr, Readarr, Lidarr, Jellyseerr).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The app being migrated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Radarr,
    Sonarr,
    Prowlarr,
    Readarr,
    Lidarr,
    Jellyseerr,
}

impl SourceKind {
    pub const ALL: [SourceKind; 6] = [
        SourceKind::Radarr,
        SourceKind::Sonarr,
        SourceKind::Prowlarr,
        SourceKind::Readarr,
        SourceKind::Lidarr,
        SourceKind::Jellyseerr,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Radarr => "Radarr",
            SourceKind::Sonarr => "Sonarr",
            SourceKind::Prowlarr => "Prowlarr",
            SourceKind::Readarr => "Readarr",
            SourceKind::Lidarr => "Lidarr",
            SourceKind::Jellyseerr => "Jellyseerr",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Radarr => "radarr",
            SourceKind::Sonarr => "sonarr",
            SourceKind::Prowlarr => "prowlarr",
            SourceKind::Readarr => "readarr",
            SourceKind::Lidarr => "lidarr",
            SourceKind::Jellyseerr => "jellyseerr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// The default port, for the connection form's placeholder.
    pub fn default_port(self) -> u16 {
        match self {
            SourceKind::Radarr => 7878,
            SourceKind::Sonarr => 8989,
            SourceKind::Prowlarr => 9696,
            SourceKind::Readarr => 8787,
            SourceKind::Lidarr => 8686,
            SourceKind::Jellyseerr => 5055,
        }
    }
}

/// What kind of thing an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MigrateCategory {
    Tag,
    RootFolder,
    QualityProfile,
    Indexer,
    DownloadClient,
    RemotePathMapping,
    Movie,
    Series,
    Author,
    Artist,
    User,
    Request,
}

impl MigrateCategory {
    pub fn label(self) -> &'static str {
        match self {
            MigrateCategory::Tag => "Tags",
            MigrateCategory::RootFolder => "Root folders",
            MigrateCategory::QualityProfile => "Quality profiles",
            MigrateCategory::Indexer => "Indexers",
            MigrateCategory::DownloadClient => "Download clients",
            MigrateCategory::RemotePathMapping => "Remote path mappings",
            MigrateCategory::Movie => "Movies",
            MigrateCategory::Series => "Series",
            MigrateCategory::Author => "Authors",
            MigrateCategory::Artist => "Artists",
            MigrateCategory::User => "Users",
            MigrateCategory::Request => "Requests",
        }
    }
}

/// What the migration does (or did) with one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MigrateAction {
    /// Will be (or was) created in Reelix.
    Create,
    /// Already in Reelix; the existing one is used.
    Existing,
    /// Can't be migrated; `detail` says why.
    Skip,
    /// Creating it failed (only in a report).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigrateItem {
    /// Stable id of the item in its source (`indexer:3`), used to supply hidden secrets.
    pub key: String,
    pub category: MigrateCategory,
    pub name: String,
    pub action: MigrateAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Secret settings the source hides (shown as `********`) that must be entered to migrate
    /// this item; supply them in [`MigrateConnection::secrets`] as `"<key>:<field>"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_secrets: Vec<String>,
}

/// Rewrites a path prefix, for when Reelix sees the media under a different path than the
/// app being migrated (e.g. different Docker volume mounts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PathRewrite {
    pub from: String,
    pub to: String,
}

/// Where to read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigrateConnection {
    pub source: SourceKind,
    /// e.g. `http://192.168.1.10:7878` (include any URL base).
    pub url: String,
    pub api_key: String,
    #[serde(default)]
    pub path_rewrites: Vec<PathRewrite>,
    /// Values for secrets the source hides, keyed `"<item key>:<field>"`.
    #[serde(default)]
    pub secrets: std::collections::BTreeMap<String, String>,
}

/// A preview of what a migration would do; nothing is changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigratePlan {
    pub source: SourceKind,
    pub version: String,
    pub items: Vec<MigrateItem>,
    /// Things to know before running (e.g. masked secrets, missing metadata keys).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigrateRunRequest {
    #[serde(flatten)]
    pub connection: MigrateConnection,
    /// Categories to import; empty means all.
    #[serde(default)]
    pub categories: Vec<MigrateCategory>,
    /// Start searching for monitored items that have no files once they're added.
    #[serde(default)]
    pub search_missing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MigrateState {
    Running,
    Finished,
    Failed,
}

/// Progress and result of the current or last migration run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigrateRunDto {
    pub source: SourceKind,
    pub state: MigrateState,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Items handled so far and in total.
    pub done: usize,
    pub total: usize,
    /// What it's working on now.
    pub current: Option<String>,
    /// Outcome per item (`Create` means created).
    pub items: Vec<MigrateItem>,
    /// Set when the whole run failed (e.g. the source became unreachable).
    pub error: Option<String>,
}

/// What [`PendingMigrationSource::api_key`] shows; sending it back as
/// [`MigrateConnection::api_key`] (with the same source and URL) uses the saved key.
pub const SAVED_API_KEY: &str = "********";

/// An app Setup (the Windows installer, `reelix migrate prepare`) found and saved for the admin
/// to migrate from; the key is stored encrypted and never returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PendingMigrationSource {
    pub source: SourceKind,
    pub url: String,
    /// Always [`SAVED_API_KEY`].
    pub api_key: String,
    pub added_at: DateTime<Utc>,
}
