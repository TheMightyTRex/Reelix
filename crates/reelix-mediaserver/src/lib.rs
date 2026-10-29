//! Media server adapters: Jellyfin, Emby, Plex (auth, library refresh, availability, watchlists)
//!
//! Jellyfin and Emby share one implementation ([`emby::EmbyLike`]) that differs only in
//! auth headers. Plex talks to the server with an `X-Plex-Token`; account-level features
//! (PIN login, server discovery, watchlist) live in [`plextv::PlexTv`].

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reelix_core::provider::ProviderSchema;
use reelix_core::{ContentKind, MediaKind};
use serde::{Deserialize, Serialize};

pub mod emby;
pub mod error;
pub mod plex;
pub mod plextv;
mod settings;

pub use error::MsError;

/// Which product a server is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaServerKind {
    Jellyfin,
    Emby,
    Plex,
    Audiobookshelf,
    Kavita,
    CalibreWeb,
    Navidrome,
}

/// Identity of a reachable server, returned by [`MediaServer::test`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub kind: MediaServerKind,
    pub id: String,
    pub name: String,
    pub version: String,
}

/// A user account on (or linked to) a media server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalUser {
    /// Server-side user id (Jellyfin/Emby GUID, plex.tv numeric id).
    pub id: String,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Per-user access token when obtained through a login (never persisted in plain text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
}

/// A movie or series in the server's library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MsItem {
    pub kind: MediaKind,
    /// Server item id / Plex ratingKey.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<DateTime<Utc>>,
}

/// One watched movie or episode. For episodes, external ids are the episode's own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchedItem {
    /// Movie or (for episodes) Series. External ids are the movie's or the episode's own.
    pub kind: MediaKind,
    pub id: String,
    /// Movie title, or the episode title for series.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_title: Option<String>,
    /// Server id of the parent series (episodes only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub play_count: u32,
}

/// A configured media server.
#[async_trait]
pub trait MediaServer: Send + Sync {
    fn kind(&self) -> MediaServerKind;

    /// Checks connectivity and credentials.
    async fn test(&self) -> Result<ServerInfo, MsError>;

    /// Username/password login (Jellyfin/Emby). Plex returns [`MsError::Unsupported`]; use
    /// the plex.tv PIN flow in [`plextv::PlexTv`] instead.
    async fn authenticate(&self, username: &str, password: &str) -> Result<ExternalUser, MsError>;

    /// Accounts with access to this server.
    async fn users(&self) -> Result<Vec<ExternalUser>, MsError>;

    /// Asks the server to rescan. Empty `paths` refreshes everything; otherwise only the
    /// given paths (or the libraries containing them) where the server supports it.
    async fn refresh_library(&self, paths: &[PathBuf]) -> Result<(), MsError>;

    /// All movies and series in the server's libraries.
    async fn library_items(&self) -> Result<Vec<MsItem>, MsError>;

    /// Watched movies/episodes for one server user id, newest first where available.
    async fn watch_history(&self, user_id: &str) -> Result<Vec<WatchedItem>, MsError>;

    /// Content this server serves; the pipeline only refreshes servers that serve the imported kind.
    fn content_kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Movie, ContentKind::Series]
    }
}

/// Shared dependencies handed to every adapter at build time.
#[derive(Debug, Clone)]
pub struct BuildContext {
    pub http: reqwest::Client,
    /// Stable per-installation id (`DeviceId` / `X-Plex-Client-Identifier`).
    pub client_id: String,
    /// Product name sent to servers.
    pub product: String,
    pub version: String,
}

impl BuildContext {
    pub fn new(http: reqwest::Client, client_id: impl Into<String>) -> Self {
        Self {
            http,
            client_id: client_id.into(),
            product: "Reelix".into(),
            version: reelix_core::VERSION.into(),
        }
    }
}

/// One schema per implementation.
pub fn schemas() -> Vec<ProviderSchema> {
    vec![
        emby::schema(MediaServerKind::Jellyfin),
        emby::schema(MediaServerKind::Emby),
        plex::schema(),
    ]
}

/// Builds an adapter from its implementation id (`jellyfin`, `emby`, `plex`) and settings.
pub fn build(
    implementation: &str,
    settings: &serde_json::Value,
    ctx: &BuildContext,
) -> Result<Box<dyn MediaServer>, MsError> {
    Ok(match implementation {
        "jellyfin" => Box::new(emby::EmbyLike::from_settings(
            MediaServerKind::Jellyfin,
            settings,
            ctx,
        )?),
        "emby" => Box::new(emby::EmbyLike::from_settings(
            MediaServerKind::Emby,
            settings,
            ctx,
        )?),
        "plex" => Box::new(plex::Plex::from_settings(settings, ctx)?),
        other => return Err(MsError::UnknownImplementation(other.into())),
    })
}

/// Parses `tmdb`/`tvdb`/`imdb` provider ids from `(key, value)` pairs (case-insensitive keys).
pub(crate) fn provider_ids<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> (Option<i64>, Option<i64>, Option<String>) {
    let (mut tmdb, mut tvdb, mut imdb) = (None, None, None);
    for (k, v) in pairs {
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "tmdb" => tmdb = v.parse().ok().or(tmdb),
            "tvdb" => tvdb = v.parse().ok().or(tvdb),
            "imdb" if v.starts_with("tt") => imdb = Some(v.to_string()),
            _ => {}
        }
    }
    (tmdb, tvdb, imdb)
}
