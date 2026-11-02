//! DTOs for the search → grab → download → import pipeline (M4).
//!
//! Routes (all `/api/v1`):
//! - `GET /releases?movie_id=|series_id=&season=|episode_id=|book_id=[&book_format=]|album_id=` → `Vec<ReleaseDto>` (interactive search; MANAGE_LIBRARY)
//! - `POST /releases/grab` `GrabRequest` → `GrabResponse`
//! - `GET /queue?page&page_size` → `Paged<QueueItemDto>`; `DELETE /queue/{id}?remove_from_client&blocklist&skip_redownload`
//! - `POST /queue/{id}/import` (retry import) · `POST /queue/{id}/ignore`
//! - `GET /history?page&page_size&movie_id&series_id&episode_id&book_id&author_id&album_id&artist_id&event_type` → `Paged<HistoryDto>`; `POST /history/{id}/failed` (mark failed → blocklist + re-search)
//! - `GET /blocklist?page&page_size` → `Paged<BlocklistDto>`; `DELETE /blocklist/{id}`; `DELETE /blocklist` (bulk `{ids}` or all)
//! - `GET /pending` → `Vec<PendingReleaseDto>`; `DELETE /pending/{id}`; `POST /pending/{id}/grab`
//! - `GET /manualimport?folder=|download_id=` → `Vec<ManualImportItemDto>`; `POST /manualimport` `ManualImportRequest` → 202 `EnqueuedJob`
//!
//! Live updates: `entity.changed` with entity `queue`, `history`, `blocklist`, `pending`.

use chrono::{DateTime, Utc};
use reelix_core::{BookFormat, Protocol};
use reelix_parser::{Language, QualityModel};
use serde::{Deserialize, Serialize};

pub use crate::library::Paged;
pub use crate::profiles::RejectionDto;

/// A custom format matched by a release or file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FormatRefDto {
    pub id: i64,
    pub name: String,
}

/// Minimal media reference shown next to releases, queue and history rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MediaRefDto {
    /// `movie`, `series`, `book` or `album`.
    pub kind: String,
    /// Movie / series / book / album id.
    pub id: i64,
    pub title: String,
    pub year: Option<i32>,
    /// Author (book) or artist (album).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Book format the row is about (`ebook` / `audiobook`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub book_format: Option<BookFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EpisodeRefDto {
    pub id: i64,
    pub season_number: u32,
    pub episode_number: u32,
    pub title: Option<String>,
}

// ---------- Interactive search ----------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ReleaseQuery {
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub season: Option<u32>,
    pub episode_id: Option<i64>,
    /// Book search (one format; defaults to the first format the author has a profile for).
    pub book_id: Option<i64>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub book_format: Option<BookFormat>,
    pub album_id: Option<i64>,
}

/// A release found by an interactive search, with the decision for the target media.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseDto {
    pub guid: String,
    pub title: String,
    pub indexer_id: i64,
    pub indexer: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub protocol: Protocol,
    pub size: Option<u64>,
    pub publish_date: Option<DateTime<Utc>>,
    /// Age in minutes at search time.
    pub age_minutes: Option<i64>,
    pub seeders: Option<u32>,
    pub peers: Option<u32>,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub quality: QualityModel,
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub custom_formats: Vec<FormatRefDto>,
    pub format_score: i32,
    pub indexer_flags: Vec<String>,
    pub info_url: Option<String>,
    pub accepted: bool,
    pub rejections: Vec<RejectionDto>,
    /// Higher is better; the order the automatic search would grab in.
    pub rank: u32,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episodes: Vec<EpisodeRefDto>,
    pub full_season: bool,
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub book_format: Option<BookFormat>,
    #[serde(default)]
    pub album_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GrabRequest {
    pub guid: String,
    pub indexer_id: i64,
    /// Grab even if the decision rejected it.
    #[serde(default)]
    pub force: bool,
    /// Optional overrides when the release was mapped to the wrong media.
    #[serde(default)]
    pub movie_id: Option<i64>,
    #[serde(default)]
    pub episode_ids: Option<Vec<i64>>,
    /// Use this download client instead of the automatic choice.
    #[serde(default)]
    pub download_client_id: Option<i64>,
    /// Raw grab (indexer search page): when the guid isn't a cached interactive-search result,
    /// send this URL straight to a client. The download is tracked without media until it is
    /// imported manually.
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GrabResponse {
    /// Tracked download (queue item) id.
    pub queue_id: i64,
    pub download_client: String,
    pub download_id: String,
}

// ---------- Queue ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QueueItemDto {
    pub id: i64,
    pub title: String,
    pub download_client_id: i64,
    pub download_client: String,
    pub download_id: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub protocol: Protocol,
    /// Tracked state: downloading, import_pending, importing, failed_pending.
    pub state: String,
    /// Client state: queued, downloading, paused, post_processing, completed, failed, warning.
    pub client_state: Option<String>,
    /// 0.0–1.0
    pub progress: f64,
    pub size: Option<u64>,
    pub remaining: Option<u64>,
    pub eta_secs: Option<u64>,
    pub media: Option<MediaRefDto>,
    pub episodes: Vec<EpisodeRefDto>,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub quality: Option<QualityModel>,
    pub custom_formats: Vec<FormatRefDto>,
    pub format_score: i32,
    pub indexer: Option<String>,
    /// Warnings / reasons the import is blocked.
    pub messages: Vec<String>,
    pub output_path: Option<String>,
    /// Manual import is possible (completed but not imported).
    pub can_manual_import: bool,
    pub added: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct QueueRemoveQuery {
    #[serde(default)]
    pub remove_from_client: bool,
    #[serde(default)]
    pub blocklist: bool,
    /// With `blocklist`: don't search for a replacement.
    #[serde(default)]
    pub skip_redownload: bool,
}

/// `DELETE /queue/bulk`: remove several queue items with the same options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct QueueBulkRemove {
    pub ids: Vec<i64>,
    #[serde(default)]
    pub remove_from_client: bool,
    #[serde(default)]
    pub blocklist: bool,
    #[serde(default)]
    pub skip_redownload: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QueueBulkRemoveResult {
    pub removed: u32,
    /// Items that could not be removed; the others were.
    pub failed: Vec<QueueBulkFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QueueBulkFailure {
    pub id: i64,
    pub error: String,
}

// ---------- History / blocklist / pending ----------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct HistoryQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_id: Option<i64>,
    pub book_id: Option<i64>,
    pub author_id: Option<i64>,
    pub album_id: Option<i64>,
    pub artist_id: Option<i64>,
    /// grabbed, imported, download_failed, import_failed, file_deleted, renamed, ignored
    pub event_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HistoryDto {
    pub id: i64,
    pub event_type: String,
    pub date: DateTime<Utc>,
    pub media: Option<MediaRefDto>,
    pub episode: Option<EpisodeRefDto>,
    pub source_title: String,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub quality: Option<QualityModel>,
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub languages: Vec<Language>,
    pub custom_formats: Vec<FormatRefDto>,
    pub format_score: i32,
    pub indexer: Option<String>,
    pub download_client: Option<String>,
    pub download_id: Option<String>,
    /// Event details (paths, messages, release info).
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct PageQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BlocklistDto {
    pub id: i64,
    pub media: Option<MediaRefDto>,
    pub source_title: String,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub quality: Option<QualityModel>,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub protocol: Option<Protocol>,
    pub indexer: Option<String>,
    pub message: Option<String>,
    pub date: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BulkIdsRequest {
    /// Empty = all.
    #[serde(default)]
    pub ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PendingReleaseDto {
    pub id: i64,
    pub title: String,
    pub media: Option<MediaRefDto>,
    pub episodes: Vec<EpisodeRefDto>,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub quality: QualityModel,
    pub indexer: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub protocol: Protocol,
    /// `delay` or `preferred_protocol`.
    pub reason: String,
    pub added: DateTime<Utc>,
    pub release_at: DateTime<Utc>,
}

// ---------- Manual import ----------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ManualImportQuery {
    /// A folder to scan (must be inside a root folder, a download client output dir or a mapped path).
    pub folder: Option<String>,
    /// Or: a queue item's download id.
    pub download_id: Option<String>,
    /// Restrict matching to this movie/series.
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ManualImportItemDto {
    pub path: String,
    pub relative_path: String,
    pub size: u64,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub quality: QualityModel,
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub media: Option<MediaRefDto>,
    pub season_number: Option<u32>,
    pub episodes: Vec<EpisodeRefDto>,
    /// Why this file can't be imported as-is (unknown media, not an upgrade, sample…).
    pub rejections: Vec<String>,
    pub download_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ManualImportFile {
    pub path: String,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    #[serde(default)]
    pub episode_ids: Vec<i64>,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub quality: Option<QualityModel>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub download_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ManualImportRequest {
    pub files: Vec<ManualImportFile>,
    /// `auto` (settings), `move`, `copy` or `hardlink`.
    #[serde(default = "auto")]
    pub import_mode: String,
}

fn auto() -> String {
    "auto".into()
}
