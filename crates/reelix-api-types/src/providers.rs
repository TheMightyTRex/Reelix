//! Providers: indexers, download clients, notifications, media servers, import lists,
//! tags, remote path mappings and the metadata / indexer settings.
//!
//! Every provider kind shares one CRUD shape (`/api/v1/{kind}`), described by
//! [`ProviderSchema`]s that the UI renders generically. Secret settings come back as
//! [`SECRET_MASK`]; sending the mask back unchanged keeps the stored value.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use reelix_core::Protocol;
pub use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SECRET_MASK, SelectOption};

/// The five provider families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Indexer,
    DownloadClient,
    Notification,
    MediaServer,
    ImportList,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 5] = [
        Self::Indexer,
        Self::DownloadClient,
        Self::Notification,
        Self::MediaServer,
        Self::ImportList,
    ];

    /// Database / wire id (`download_client`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indexer => "indexer",
            Self::DownloadClient => "download_client",
            Self::Notification => "notification",
            Self::MediaServer => "media_server",
            Self::ImportList => "import_list",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// API path segment (`/api/v1/downloadclients`).
    pub fn path(self) -> &'static str {
        match self {
            Self::Indexer => "indexers",
            Self::DownloadClient => "downloadclients",
            Self::Notification => "notifications",
            Self::MediaServer => "mediaservers",
            Self::ImportList => "importlists",
        }
    }

    pub fn from_path(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.path() == s)
    }

    /// Singular display name ("Download client").
    pub fn label(self) -> &'static str {
        match self {
            Self::Indexer => "Indexer",
            Self::DownloadClient => "Download client",
            Self::Notification => "Notification",
            Self::MediaServer => "Media server",
            Self::ImportList => "Import list",
        }
    }
}

/// A configured provider instance. Secrets in `settings` are masked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProviderDto {
    pub id: i64,
    pub kind: ProviderKind,
    pub implementation: String,
    /// Display name of the implementation (e.g. `qBittorrent`).
    pub implementation_name: String,
    pub name: String,
    pub enabled: bool,
    /// Lower is preferred (1–50).
    pub priority: i32,
    /// Tag ids (see `/api/v1/tags`).
    pub tags: Vec<i64>,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub settings: Value,
    /// Indexers and download clients only.
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub protocol: Option<Protocol>,
    /// Indexers only: failure back-off state.
    pub status: Option<IndexerStatusDto>,
    /// Why the instance could not be built from its settings (shown as an error badge).
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Create/replace body for any provider kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProviderInput {
    pub implementation: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub tags: Vec<i64>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub settings: Value,
}

fn yes() -> bool {
    true
}

fn default_priority() -> i32 {
    25
}

/// `POST /{kind}/test` body: unsaved settings. With `id`, masked secrets are filled in from
/// that saved instance (testing an edited form).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProviderTestRequest {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(flatten)]
    pub provider: ProviderInput,
}

/// An implementation that can be added, with catalog facets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProviderSchemaDto {
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub schema: ProviderSchema,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub protocol: Option<Protocol>,
    /// Indexers: `public`, `private`, `semi-private` (definitions) or null (generic).
    pub privacy: Option<String>,
    /// Indexers: site language from the definition.
    pub language: Option<String>,
    /// `builtin` or `definition`.
    pub source: String,
}

/// One step of a connection test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TestCheckDto {
    pub name: String,
    pub ok: bool,
    pub message: Option<String>,
}

/// Outcome of a provider / metadata test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TestResultDto {
    pub ok: bool,
    /// First failure, or a success detail (e.g. server name and version).
    pub message: Option<String>,
    pub checks: Vec<TestCheckDto>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IndexerStatusDto {
    pub failure_count: u32,
    pub disabled_until: Option<DateTime<Utc>>,
    pub last_failure_reason: Option<String>,
}

/// `GET /indexers/stats` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IndexerStatsDto {
    pub indexer_id: i64,
    pub name: String,
    pub enabled: bool,
    pub queries: u64,
    pub rss_queries: u64,
    pub grabs: u64,
    pub failures: u64,
    pub avg_response_ms: u64,
    pub status: IndexerStatusDto,
}

/// One release from a manual indexer search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseDto {
    pub guid: String,
    pub title: String,
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    pub info_url: Option<String>,
    pub info_hash: Option<String>,
    pub size: Option<u64>,
    pub publish_date: Option<DateTime<Utc>>,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub protocol: Protocol,
    pub indexer_id: i64,
    pub indexer: String,
    pub categories: Vec<u32>,
    pub seeders: Option<u32>,
    pub peers: Option<u32>,
    pub grabs: Option<u32>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub download_volume_factor: f64,
    pub upload_volume_factor: f64,
    /// `freeleech`, `halfleech`, `double_upload`, `internal`, `scene`, `nuked`, `free_upload_slot`.
    pub indexer_flags: Vec<String>,
    /// Soulseek: the peer and its files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub soulseek: Option<reelix_core::soulseek::SoulseekRelease>,
}

/// Per-indexer outcome of a manual search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IndexerSearchOutcome {
    pub indexer_id: i64,
    pub name: String,
    pub count: usize,
    pub elapsed_ms: u64,
    /// Set when the indexer failed.
    pub error: Option<String>,
}

/// `GET /indexers/search` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IndexerSearchResponse {
    pub releases: Vec<ReleaseDto>,
    pub indexers: Vec<IndexerSearchOutcome>,
}

/// A Newznab category, for the search page category picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CategoryDto {
    pub id: u32,
    pub name: String,
    #[cfg_attr(feature = "openapi", schema(no_recursion))]
    pub subcategories: Vec<CategoryDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagDto {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagInput {
    pub label: String,
}

/// Maps a download client's view of a path to the local one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RemotePathMappingDto {
    pub id: i64,
    /// Download client host as configured in its settings.
    pub host: String,
    pub remote_path: String,
    pub local_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RemotePathMappingInput {
    pub host: String,
    pub remote_path: String,
    pub local_path: String,
}

/// `GET/PUT /settings/metadata`. Secrets are masked on read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct MetadataSettingsDto {
    pub tmdb_api_key: Option<String>,
    pub tmdb_read_token: Option<String>,
    pub tvdb_api_key: Option<String>,
    pub tvdb_pin: Option<String>,
    pub language: String,
    pub region: Option<String>,
}

impl Default for MetadataSettingsDto {
    fn default() -> Self {
        Self {
            tmdb_api_key: None,
            tmdb_read_token: None,
            tvdb_api_key: None,
            tvdb_pin: None,
            language: "en-US".into(),
            region: None,
        }
    }
}

/// Proxy used for indexer traffic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProxyDto {
    /// `http` or `socks5`.
    pub kind: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    /// Masked on read.
    #[serde(default)]
    pub password: Option<String>,
}

/// `GET/PUT /settings/indexers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct IndexerSettingsDto {
    /// RSS sync interval in minutes (0 disables; otherwise 10–120).
    pub rss_interval_minutes: u32,
    /// Allow indexers, notifications and import lists to reach loopback/private addresses.
    pub allow_private_addresses: bool,
    pub flaresolverr_url: Option<String>,
    pub proxy: Option<ProxyDto>,
}

impl Default for IndexerSettingsDto {
    fn default() -> Self {
        Self {
            rss_interval_minutes: 15,
            allow_private_addresses: false,
            flaresolverr_url: None,
            proxy: None,
        }
    }
}

/// `POST /mediaservers/plex/pin` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlexPinDto {
    pub id: i64,
    pub code: String,
    /// Open this in a popup; the user signs in to plex.tv there.
    pub auth_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlexServerDto {
    pub name: String,
    pub owned: bool,
    /// Connection URIs, local ones first.
    pub uris: Vec<String>,
}

/// `GET /mediaservers/plex/pin/{id}` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlexPinStatusDto {
    pub authorized: bool,
    /// The account token once authorized (paste into the server's token field).
    pub token: Option<String>,
    /// Servers on the account (when authorized).
    pub servers: Vec<PlexServerDto>,
}

/// Default source of the community indexer definition catalog (downloaded at runtime,
/// never shipped with Reelix).
pub const DEFAULT_DEFINITIONS_SOURCE: &str =
    "https://codeload.github.com/Prowlarr/Indexers/tar.gz/refs/heads/master";

/// `GET/PUT /settings/definitions`: runtime sync of the indexer definition catalog.
/// `last_*`, `etag`, `commit` and `upstream_count` are maintained by the sync and ignored on PUT.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct DefinitionsSettingsDto {
    /// Automatic updates (the `definitions_sync` task). "Sync now" works either way.
    pub enabled: bool,
    /// `.tar.gz` of a definitions repository (http/https).
    pub source_url: String,
    /// Schema directory inside the archive (`*/definitions/{schema_version}/*.yml`).
    pub schema_version: String,
    /// Hours between automatic checks (1–720).
    pub auto_update_hours: u32,
    /// Last time new definitions were installed.
    pub last_sync: Option<DateTime<Utc>>,
    /// Last successful check (including "not modified").
    pub last_check: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub etag: Option<String>,
    /// Source commit, when the archive records one.
    pub commit: Option<String>,
    /// Definition files installed by the last sync.
    pub upstream_count: usize,
}

impl Default for DefinitionsSettingsDto {
    fn default() -> Self {
        Self {
            enabled: true,
            source_url: DEFAULT_DEFINITIONS_SOURCE.into(),
            schema_version: "v11".into(),
            auto_update_hours: 24,
            last_sync: None,
            last_check: None,
            last_error: None,
            etag: None,
            commit: None,
            upstream_count: 0,
        }
    }
}

/// Where a definition file comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum DefinitionSource {
    /// Managed by the sync (`definitions/upstream`).
    Upstream,
    /// User-provided (`definitions/custom`); overrides upstream ids.
    Custom,
}

/// A definition file that failed to load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DefinitionLoadErrorDto {
    /// Definition id (file stem).
    pub id: String,
    /// File name.
    pub file: String,
    pub source: DefinitionSource,
    pub error: String,
}

/// `GET /indexers/definitions/status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DefinitionsStatusDto {
    pub enabled: bool,
    pub source_url: String,
    pub schema_version: String,
    pub auto_update_hours: u32,
    pub last_sync: Option<DateTime<Utc>>,
    pub last_check: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub commit: Option<String>,
    /// Definition files in `definitions/upstream`.
    pub upstream_count: usize,
    /// Definition files in `definitions/custom`.
    pub custom_count: usize,
    /// Upstream definitions replaced by a custom one with the same id.
    pub overridden: usize,
    /// Definitions loaded and available in the Add Indexer catalog.
    pub loaded: usize,
    pub failed: Vec<DefinitionLoadErrorDto>,
    /// Absolute path of the custom definitions folder.
    pub custom_dir: String,
    /// A sync is running right now.
    pub syncing: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_roundtrip() {
        for k in ProviderKind::ALL {
            assert_eq!(ProviderKind::parse(k.as_str()), Some(k));
            assert_eq!(ProviderKind::from_path(k.path()), Some(k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
    }

    #[test]
    fn input_defaults_and_flattened_test_request() {
        let r: ProviderTestRequest =
            serde_json::from_value(serde_json::json!({"implementation": "x", "name": "n", "id": 3})).unwrap();
        assert_eq!(r.id, Some(3));
        assert!(r.provider.enabled);
        assert_eq!(r.provider.priority, 25);
        assert!(r.provider.settings.is_null());
    }
}
