//! DTOs for M7 integrations: import list sync + exclusions and media server availability.
//!
//! Routes (all `/api/v1`, admin only):
//! - `GET /importlists/exclusions` → `Vec<ImportListExclusionDto>`; `POST /importlists/exclusions`
//!   `CreateExclusionRequest` → 201 `ImportListExclusionDto`; `DELETE /importlists/exclusions/{id}`;
//!   `DELETE /importlists/exclusions` `BulkIdsRequest` (empty = all)
//! - `POST /importlists/sync` `ImportListSyncRequest` → 202 `EnqueuedJob` (job `import_list_sync`)
//! - `GET /importlists/status` → `Vec<ImportListStatusDto>`
//! - `GET /importlists/kinds` → `Vec<ImportListKindDto>` (which implementations yield music)
//! - `GET /importlists/musicexclusions` → `Vec<MusicListExclusionDto>`; `POST
//!   /importlists/musicexclusions` `CreateMusicExclusionRequest` → 201; `DELETE
//!   /importlists/musicexclusions/{id}`; `DELETE /importlists/musicexclusions` `BulkIdsRequest`
//! - `GET /mediaservers/availability` → `MediaServerAvailabilityDto`
//!
//! Music lists (Last.fm, ListenBrainz, Spotify) carry music target fields instead of the
//! movie/series ones: `quality_profile_id` (music family), `metadata_profile_id`,
//! `root_folder_id` (music root folder), `artist_monitor`, `monitor_new_items`,
//! `album_handling` (`album` | `artist`) and `search_on_add`.
//!
//! Live updates: `entity.changed` with entity `import_list_exclusion`, `music_list_exclusion`,
//! `import_list`
//! (action `synced`) and `media_server_availability` (action `synced`).

use chrono::{DateTime, Utc};
use reelix_core::MediaKind;
use serde::{Deserialize, Serialize};

pub use crate::pipeline::BulkIdsRequest;

/// Media that import lists will never add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportListExclusionDto {
    pub id: i64,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub title: String,
    pub year: Option<i32>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateExclusionRequest {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub title: String,
    #[serde(default)]
    pub year: Option<i32>,
}

/// What an import-list implementation yields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportListKindDto {
    pub implementation: String,
    /// `video` (movies/series) or `music` (artists/albums).
    pub content: String,
}

/// Artist or album (release group) exclusion of the music import lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum MusicExclusionKind {
    Artist,
    Album,
}

/// An artist / album music import lists will never add (an excluded artist also blocks its
/// albums).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MusicListExclusionDto {
    pub id: i64,
    pub kind: MusicExclusionKind,
    /// Artist MBID or release-group MBID.
    pub musicbrainz_id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateMusicExclusionRequest {
    pub kind: MusicExclusionKind,
    pub musicbrainz_id: String,
    pub name: String,
}

/// Run an import list sync now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportListSyncRequest {
    /// Lists to sync (ignoring their interval); empty/absent = every enabled list.
    #[serde(default)]
    pub list_ids: Vec<i64>,
}

/// Outcome of the last sync of one import list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportListStatusDto {
    pub list_id: i64,
    pub last_sync: DateTime<Utc>,
    pub last_error: Option<String>,
    pub items_found: u32,
    pub items_added: u32,
}

/// Library items found on media servers by the availability sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MediaServerAvailabilityDto {
    pub movies_total: u32,
    /// Library movies found on at least one server.
    pub movies_available: u32,
    pub series_total: u32,
    pub series_available: u32,
    pub servers: Vec<MediaServerSyncDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MediaServerSyncDto {
    pub server_id: i64,
    pub name: String,
    pub synced_at: Option<DateTime<Utc>>,
    /// Items reported by the server.
    pub items_total: u32,
    pub movies_matched: u32,
    pub series_matched: u32,
    pub last_error: Option<String>,
}
