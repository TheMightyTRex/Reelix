//! Movie & series library (M2/M5): movies, series, seasons, episodes, files, collections,
//! root folders, library import, renaming, calendar, wanted lists, naming and
//! media-management settings and library statistics.
//!
//! All routes are under `/api/v1`. Reads and writes need `MANAGE_LIBRARY`; settings need `ADMIN`.
//! List endpoints that can grow large return [`Paged`]; omit `page_size` to get every item.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

pub use reelix_core::MediaKind;
pub use reelix_parser::{Language, Quality, QualityModel};

fn yes() -> bool {
    true
}

// ======================================================================
// Common
// ======================================================================

/// One page of a list. `page` is 1-based; `page_size` 0 means "everything".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
}

/// Sort direction for list endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

/// Primary artwork as absolute URLs (`image.tmdb.org`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImagesDto {
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub logo: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RatingsDto {
    /// 0–10.
    pub vote_average: f64,
    pub vote_count: u32,
    pub popularity: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CastDto {
    pub tmdb_id: u32,
    pub name: String,
    pub character: Option<String>,
    pub profile_url: Option<String>,
}

/// How tag lists in bulk edits are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum TagMode {
    #[default]
    Replace,
    Add,
    Remove,
}

/// Technical facts about a media file (from ffprobe).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MediaInfoDto {
    pub container: String,
    pub duration_secs: Option<f64>,
    pub overall_bitrate: Option<u64>,
    /// e.g. `x265`, `AVC`.
    pub video_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bit_depth: Option<u8>,
    pub fps: Option<f64>,
    pub video_bitrate: Option<u64>,
    /// e.g. `["HDR10", "DV"]`.
    pub hdr: Vec<String>,
    /// e.g. `TrueHD Atmos`.
    pub audio_codec: Option<String>,
    pub audio_channels: Option<f32>,
    pub audio_stream_count: u32,
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
}

/// Fields shared by movie and episode files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MediaFileDto {
    pub id: i64,
    /// Path relative to the movie/series folder.
    pub relative_path: String,
    /// Absolute path on disk.
    pub path: String,
    pub size: u64,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub quality: QualityModel,
    /// Display name of the quality, e.g. `Bluray-1080p`.
    pub quality_name: String,
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub edition: Option<String>,
    /// Release (scene) name the file was imported from, when known.
    pub scene_name: Option<String>,
    /// Path the file was imported from, when known.
    pub original_path: Option<String>,
    pub media_info: Option<MediaInfoDto>,
    pub custom_format_ids: Vec<i64>,
    pub custom_formats: Vec<String>,
    pub custom_format_score: i32,
    /// File meets the quality profile's quality + format-score cutoff.
    pub cutoff_met: bool,
    pub date_added: DateTime<Utc>,
}

// ======================================================================
// Movies
// ======================================================================

/// Radarr-style movie status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MovieStatus {
    /// Rumored/planned, no dates yet.
    Tba,
    Announced,
    InCinemas,
    Released,
    /// Removed from TMDB.
    Deleted,
}

/// When a movie counts as available for searching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MinimumAvailability {
    Announced,
    InCinemas,
    #[default]
    Released,
}

/// What to monitor when adding a movie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MovieMonitor {
    #[default]
    MovieOnly,
    /// Also monitor the movie's collection (adds the other movies of it).
    MovieAndCollection,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CollectionRefDto {
    /// Library collection id.
    pub id: i64,
    pub tmdb_id: u32,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MovieFileDto {
    pub movie_id: i64,
    #[serde(flatten)]
    pub file: MediaFileDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MovieDto {
    pub id: i64,
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub sort_title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub status: MovieStatus,
    pub runtime_minutes: Option<u32>,
    pub studio: Option<String>,
    pub genres: Vec<String>,
    pub certification: Option<String>,
    /// ISO 639-1, e.g. `en`.
    pub original_language: Option<String>,
    pub images: ImagesDto,
    pub ratings: RatingsDto,
    pub alternative_titles: Vec<String>,
    pub in_cinemas: Option<NaiveDate>,
    pub digital_release: Option<NaiveDate>,
    pub physical_release: Option<NaiveDate>,
    pub youtube_trailer: Option<String>,
    pub homepage: Option<String>,
    pub monitored: bool,
    pub minimum_availability: MinimumAvailability,
    /// Released according to `minimum_availability`.
    pub is_available: bool,
    pub quality_profile_id: i64,
    pub root_folder_id: i64,
    /// Absolute movie folder.
    pub path: String,
    pub collection: Option<CollectionRefDto>,
    pub tags: Vec<i64>,
    pub added_by_request_id: Option<i64>,
    pub has_file: bool,
    pub size_on_disk: u64,
    /// The movie's primary file.
    pub movie_file: Option<MovieFileDto>,
    /// Top-billed cast; filled on `GET /movies/{id}` only.
    #[serde(default)]
    pub cast: Vec<CastDto>,
    pub added: DateTime<Utc>,
    pub last_refreshed: Option<DateTime<Utc>>,
    /// Added with the owner's own details (`POST /customitems`): no outside match, and metadata
    /// refreshes keep the typed details.
    #[serde(default)]
    pub custom: bool,
}

/// `GET /movies` query (all filters optional).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct MovieListQuery {
    pub page: Option<u32>,
    /// Omit (or 0) for all items.
    pub page_size: Option<u32>,
    /// `title` (default), `year`, `added`, `size`, `release`, `status`, `rating`, `runtime`.
    pub sort: Option<String>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub dir: Option<SortDir>,
    /// Case-insensitive title search.
    pub q: Option<String>,
    pub monitored: Option<bool>,
    pub has_file: Option<bool>,
    /// Monitored, available and without a file.
    pub missing: Option<bool>,
    pub quality_profile_id: Option<i64>,
    /// File quality id (see `Quality::id`).
    pub quality: Option<u16>,
    pub tag: Option<i64>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub status: Option<MovieStatus>,
    pub collection_id: Option<i64>,
}

/// `POST /movies`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AddMovieRequest {
    pub tmdb_id: u32,
    pub quality_profile_id: i64,
    pub root_folder_id: i64,
    #[serde(default)]
    pub monitor: MovieMonitor,
    #[serde(default)]
    pub minimum_availability: MinimumAvailability,
    #[serde(default)]
    pub tags: Vec<i64>,
    /// Enqueue a search right after adding.
    #[serde(default)]
    pub search_on_add: bool,
    /// Use this existing folder instead of the naming-template folder (library import).
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub added_by_request_id: Option<i64>,
}

/// Partial update; `null`/missing fields are left unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MovieEdit {
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub quality_profile_id: Option<i64>,
    #[serde(default)]
    pub minimum_availability: Option<MinimumAvailability>,
    /// New root folder; the movie folder name is kept.
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    /// New absolute movie folder (single edit only).
    #[serde(default)]
    pub path: Option<String>,
    /// Move existing files when the folder changes.
    #[serde(default)]
    pub move_files: bool,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
    #[serde(default)]
    pub tag_mode: TagMode,
}

/// `PUT /movies/bulk`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MovieBulkEdit {
    pub ids: Vec<i64>,
    #[serde(flatten)]
    pub edit: MovieEdit,
}

/// `DELETE /movies/bulk` / `DELETE /series/bulk` body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BulkDeleteRequest {
    pub ids: Vec<i64>,
    /// Also delete the files (to the recycle bin when configured).
    #[serde(default)]
    pub delete_files: bool,
}

/// `?delete_files=true` on single deletes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct DeleteQuery {
    #[serde(default)]
    pub delete_files: bool,
}

/// A TMDB search result with library state (`GET /movies/lookup`, `GET /series/lookup`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LookupResult {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub vote_average: f64,
    pub popularity: f64,
    pub in_library: bool,
    /// Library movie/series id when `in_library`.
    pub library_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct LookupQuery {
    /// Free text; `tmdb:603`, `imdb:tt0133093` and `tvdb:81189` look up an id directly.
    pub term: String,
}

// ======================================================================
// Collections
// ======================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CollectionMovieDto {
    pub tmdb_id: u32,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub poster: Option<String>,
    /// Library id when the movie is in the library.
    pub movie_id: Option<i64>,
    pub monitored: bool,
    pub has_file: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CollectionDto {
    pub id: i64,
    pub tmdb_id: u32,
    pub title: String,
    pub overview: Option<String>,
    pub images: ImagesDto,
    /// Monitored collections add their missing movies on `collection_sync`.
    pub monitored: bool,
    pub quality_profile_id: i64,
    pub root_folder_id: i64,
    pub minimum_availability: MinimumAvailability,
    pub search_on_add: bool,
    pub tags: Vec<i64>,
    pub movies: Vec<CollectionMovieDto>,
    /// Movies of the collection not in the library.
    pub missing_movies: u32,
}

/// `PUT /collections/{id}` (partial).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CollectionEdit {
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub quality_profile_id: Option<i64>,
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub minimum_availability: Option<MinimumAvailability>,
    #[serde(default)]
    pub search_on_add: Option<bool>,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
}

// ======================================================================
// Series
// ======================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SeriesStatus {
    Continuing,
    Ended,
    Upcoming,
    /// Removed from TMDB.
    Deleted,
}

/// How a series numbers episodes (drives naming and matching).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SeriesType {
    #[default]
    Standard,
    Daily,
    Anime,
}

/// Which episodes to monitor (on add, and for the season pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SeriesMonitor {
    #[default]
    All,
    /// Episodes that haven't aired yet.
    Future,
    /// Aired episodes without files, plus future ones.
    Missing,
    /// Episodes with files, plus future ones.
    Existing,
    FirstSeason,
    LatestSeason,
    /// Only S01E01.
    Pilot,
    None,
}

/// Whether seasons added by a refresh get monitored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NewItemMonitor {
    #[default]
    All,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeasonStatistics {
    /// Monitored episodes that have aired (or have a file).
    pub episode_count: u32,
    pub episode_file_count: u32,
    /// Every episode of the season.
    pub total_episode_count: u32,
    pub monitored_episode_count: u32,
    pub size_on_disk: u64,
    pub next_airing: Option<NaiveDate>,
    pub previous_airing: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeasonDto {
    pub season_number: u32,
    pub monitored: bool,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub poster: Option<String>,
    pub air_date: Option<NaiveDate>,
    pub statistics: SeasonStatistics,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeriesStatistics {
    pub season_count: u32,
    /// Monitored aired episodes (+ any with files): the denominator of the progress bar.
    pub episode_count: u32,
    pub episode_file_count: u32,
    pub total_episode_count: u32,
    pub monitored_episode_count: u32,
    pub size_on_disk: u64,
    /// `episode_file_count / episode_count * 100` (100 when nothing is expected).
    pub percent_of_episodes: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeriesDto {
    pub id: i64,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub sort_title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub status: SeriesStatus,
    pub network: Option<String>,
    pub series_type: SeriesType,
    pub runtime_minutes: Option<u32>,
    pub genres: Vec<String>,
    pub certification: Option<String>,
    pub original_language: Option<String>,
    pub images: ImagesDto,
    pub ratings: RatingsDto,
    pub alternative_titles: Vec<String>,
    pub first_aired: Option<NaiveDate>,
    pub last_aired: Option<NaiveDate>,
    pub next_airing: Option<NaiveDate>,
    pub monitored: bool,
    pub monitor_new_items: NewItemMonitor,
    pub season_folder: bool,
    pub quality_profile_id: i64,
    pub root_folder_id: i64,
    pub path: String,
    pub tags: Vec<i64>,
    pub added_by_request_id: Option<i64>,
    pub seasons: Vec<SeasonDto>,
    pub statistics: SeriesStatistics,
    /// Filled on `GET /series/{id}` only.
    #[serde(default)]
    pub cast: Vec<CastDto>,
    pub added: DateTime<Utc>,
    pub last_refreshed: Option<DateTime<Utc>>,
    /// Added with the owner's own details (`POST /customitems`): no outside match, and metadata
    /// refreshes keep the typed details.
    #[serde(default)]
    pub custom: bool,
}

/// `GET /series` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct SeriesListQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    /// `title` (default), `year`, `added`, `size`, `next_airing`, `network`, `status`, `rating`, `progress`.
    pub sort: Option<String>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub dir: Option<SortDir>,
    pub q: Option<String>,
    pub monitored: Option<bool>,
    /// Has monitored aired episodes without files.
    pub missing: Option<bool>,
    pub quality_profile_id: Option<i64>,
    pub tag: Option<i64>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub status: Option<SeriesStatus>,
    pub network: Option<String>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub series_type: Option<SeriesType>,
}

/// `POST /series`. Give `tmdb_id` (preferred) or `tvdb_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AddSeriesRequest {
    #[serde(default)]
    pub tmdb_id: Option<u32>,
    #[serde(default)]
    pub tvdb_id: Option<u32>,
    pub quality_profile_id: i64,
    pub root_folder_id: i64,
    #[serde(default)]
    pub monitor: SeriesMonitor,
    /// Monitor exactly these seasons (overrides `monitor`; used by requests).
    #[serde(default)]
    pub seasons: Option<Vec<u32>>,
    #[serde(default)]
    pub series_type: SeriesType,
    #[serde(default = "yes")]
    pub season_folder: bool,
    #[serde(default)]
    pub monitor_new_items: NewItemMonitor,
    #[serde(default)]
    pub tags: Vec<i64>,
    /// Search for missing monitored episodes right after adding.
    #[serde(default)]
    pub search_on_add: bool,
    /// Also search for cutoff-unmet episodes.
    #[serde(default)]
    pub search_cutoff_unmet: bool,
    /// Use this existing folder (library import).
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub added_by_request_id: Option<i64>,
}

/// Partial series update.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeriesEdit {
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub quality_profile_id: Option<i64>,
    #[serde(default)]
    pub series_type: Option<SeriesType>,
    #[serde(default)]
    pub season_folder: Option<bool>,
    #[serde(default)]
    pub monitor_new_items: Option<NewItemMonitor>,
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub move_files: bool,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
    #[serde(default)]
    pub tag_mode: TagMode,
}

/// `PUT /series/bulk`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeriesBulkEdit {
    pub ids: Vec<i64>,
    #[serde(flatten)]
    pub edit: SeriesEdit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EpisodeFileDto {
    pub series_id: i64,
    pub season_number: u32,
    pub episode_ids: Vec<i64>,
    #[serde(flatten)]
    pub file: MediaFileDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EpisodeDto {
    pub id: i64,
    pub series_id: i64,
    pub season_number: u32,
    pub episode_number: u32,
    pub absolute_number: Option<u32>,
    pub scene_season_number: Option<u32>,
    pub scene_episode_number: Option<u32>,
    pub scene_absolute_number: Option<u32>,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<NaiveDate>,
    /// Air date at 00:00 UTC (no time-of-day data from TMDB).
    pub air_date_utc: Option<DateTime<Utc>>,
    pub runtime_minutes: Option<u32>,
    pub still: Option<String>,
    pub finale: Option<String>,
    pub monitored: bool,
    pub has_file: bool,
    pub episode_file_id: Option<i64>,
    pub episode_file: Option<EpisodeFileDto>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct EpisodeListQuery {
    pub season: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct EpisodeFileQuery {
    pub series_id: i64,
}

/// `PUT /episodes/monitor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EpisodeMonitorRequest {
    pub episode_ids: Vec<i64>,
    pub monitored: bool,
}

/// `PUT /seasons/monitor`: (un)monitor one season and, by default, its episodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeasonMonitorRequest {
    pub series_id: i64,
    pub season_number: u32,
    pub monitored: bool,
    #[serde(default = "yes")]
    pub include_episodes: bool,
}

/// `POST /seasonpass`: apply a monitor option to many series at once.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeasonPassRequest {
    pub series_ids: Vec<i64>,
    pub monitor: SeriesMonitor,
    /// Also set the series' own monitored flag (default: unchanged).
    #[serde(default)]
    pub monitored: Option<bool>,
}

// ======================================================================
// Root folders & library import
// ======================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnmappedFolderDto {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RootFolderDto {
    pub id: i64,
    pub path: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub media_kind: MediaKind,
    pub accessible: bool,
    pub free_space: Option<u64>,
    pub total_space: Option<u64>,
    /// Sub-folders not belonging to any movie/series.
    pub unmapped_folders: Vec<UnmappedFolderDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateRootFolderRequest {
    pub path: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub media_kind: MediaKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct LibraryImportScanQuery {
    pub root_folder_id: i64,
    /// Look up TMDB suggestions too (default). `false` lists folders only, fast; fetch the
    /// suggestions afterwards in batches with `POST /libraryimport/match`.
    #[serde(default)]
    pub lookup: Option<bool>,
}

/// Folders of a root folder to look up TMDB suggestions for (`POST /libraryimport/match`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LibraryImportMatchRequest {
    pub root_folder_id: i64,
    /// Folder paths from a scan, at most 25.
    pub paths: Vec<String>,
}

/// An unmapped folder with TMDB suggestions (`GET /libraryimport/scan`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportCandidateDto {
    pub folder: UnmappedFolderDto,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub media_kind: MediaKind,
    /// Title/year parsed from the folder name.
    pub parsed_title: String,
    pub parsed_year: Option<i32>,
    /// Video files found inside.
    pub file_count: u32,
    pub size: u64,
    /// Best guesses, best first (empty when TMDB is not configured or nothing matched).
    pub suggestions: Vec<LookupResult>,
    /// Why no suggestions could be looked up (TMDB not configured, unreachable, rate limited).
    #[serde(default)]
    pub lookup_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LibraryImportItem {
    /// Existing folder (absolute, inside the root folder).
    pub path: String,
    pub tmdb_id: u32,
    pub quality_profile_id: i64,
    #[serde(default = "yes")]
    pub monitored: bool,
    #[serde(default)]
    pub minimum_availability: MinimumAvailability,
    #[serde(default)]
    pub monitor: SeriesMonitor,
    #[serde(default)]
    pub series_type: SeriesType,
    #[serde(default = "yes")]
    pub season_folder: bool,
    #[serde(default)]
    pub tags: Vec<i64>,
}

/// `POST /libraryimport`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LibraryImportRequest {
    pub root_folder_id: i64,
    pub items: Vec<LibraryImportItem>,
    #[serde(default)]
    pub search_on_add: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportErrorDto {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LibraryImportResult {
    /// Ids of the created movies or series (a disk scan is queued for each).
    pub added: Vec<i64>,
    pub errors: Vec<ImportErrorDto>,
}

// ======================================================================
// Rename
// ======================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct RenamePreviewQuery {
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub season: Option<u32>,
}

/// A file whose name differs from what the naming templates produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RenamePreviewDto {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    /// Movie or series id.
    pub media_id: i64,
    pub file_id: i64,
    pub season_number: Option<u32>,
    pub episode_numbers: Vec<u32>,
    /// Relative to the movie/series folder.
    pub existing_path: String,
    pub new_path: String,
}

/// `POST /rename` → 202 with the queued `rename_files` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RenameRequest {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    /// Movie or series ids.
    pub ids: Vec<i64>,
    /// Restrict to these file ids (default: every file needing a rename).
    #[serde(default)]
    pub file_ids: Option<Vec<i64>>,
}

// ======================================================================
// Calendar & wanted
// ======================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct CalendarQuery {
    /// Inclusive, `YYYY-MM-DD` (default: today).
    pub start: Option<NaiveDate>,
    /// Inclusive, `YYYY-MM-DD` (default: start + 7 days).
    pub end: Option<NaiveDate>,
    /// Include unmonitored items (default false).
    #[serde(default)]
    pub unmonitored: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct IcalQuery {
    /// Days in the past to include (default 7).
    pub past_days: Option<u32>,
    /// Days in the future to include (default 28).
    pub future_days: Option<u32>,
    #[serde(default)]
    pub unmonitored: bool,
    /// API key (query auth for calendar apps).
    pub apikey: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CalendarKind {
    Movie,
    Episode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReleaseKind {
    Cinema,
    Digital,
    Physical,
    /// An episode air date.
    Airing,
}

/// Status used to colour calendar entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CalendarStatus {
    Downloaded,
    /// Released/aired, monitored, no file.
    Missing,
    /// Not released/aired yet.
    Upcoming,
    Unmonitored,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CalendarEntryDto {
    pub kind: CalendarKind,
    pub release_kind: ReleaseKind,
    pub date: NaiveDate,
    pub status: CalendarStatus,
    pub monitored: bool,
    pub has_file: bool,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_id: Option<i64>,
    /// Movie or series title.
    pub title: String,
    pub year: Option<i32>,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub episode_title: Option<String>,
    pub network: Option<String>,
    pub poster: Option<String>,
    /// File quality name when downloaded.
    pub quality: Option<String>,
}

/// `GET /wanted/missing` and `GET /wanted/cutoff` query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct WantedQuery {
    /// `movie` (default) or `episode`.
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub kind: Option<CalendarKind>,
    pub page: Option<u32>,
    /// Default 20; 0 = everything.
    pub page_size: Option<u32>,
    /// `date` (default: release/air date), `title`, `added`.
    pub sort: Option<String>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub dir: Option<SortDir>,
    /// Default true: only monitored items.
    pub monitored: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WantedItemDto {
    pub kind: CalendarKind,
    /// Movie id or episode id (what `POST /search` takes for `movie` / `episode`).
    pub id: i64,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    /// Movie or series title.
    pub title: String,
    pub year: Option<i32>,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub episode_title: Option<String>,
    /// Release date (movies: per minimum availability) or air date.
    pub date: Option<NaiveDate>,
    pub monitored: bool,
    pub poster: Option<String>,
    pub quality_profile_id: i64,
    /// Current file quality (cutoff-unmet list only).
    pub quality: Option<String>,
    pub custom_format_score: Option<i32>,
    /// Name of the profile's cutoff quality.
    pub cutoff: Option<String>,
}

// ======================================================================
// Search hand-off (handled by the pipeline in wave 3)
// ======================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum LibrarySearchKind {
    /// `ids` are movie ids.
    Movie,
    /// `ids` are series ids (all monitored missing episodes).
    Series,
    /// `ids` are series ids, `season` required.
    Season,
    /// `ids` are episode ids.
    Episode,
    /// Every monitored missing movie/episode (`ids` empty) or the given wanted ids.
    Missing,
    /// Every cutoff-unmet movie/episode (`ids` empty) or the given ids.
    Cutoff,
}

/// `POST /search` → 202 [`crate::EnqueuedJob`] (job kind `search`, payload = this JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchRequest {
    pub kind: LibrarySearchKind,
    #[serde(default)]
    pub ids: Vec<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    /// For `missing`/`cutoff` with empty ids: which media (`movie` default, or `episode`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<CalendarKind>,
}

// ======================================================================
// Settings
// ======================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ColonReplacement {
    Delete,
    Dash,
    SpaceDash,
    SpaceDashSpace,
    #[default]
    Smart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MultiEpisodeStyle {
    #[default]
    Extend,
    Duplicate,
    Repeat,
    Scene,
    Range,
    PrefixedRange,
}

/// `GET/PUT /settings/naming` (same JSON shape as `reelix_import::naming::NamingConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NamingSettings {
    pub rename_files: bool,
    pub replace_illegal_characters: bool,
    pub colon_replacement: ColonReplacement,
    pub movie_folder_format: String,
    pub movie_file_format: String,
    pub series_folder_format: String,
    pub season_folder_format: String,
    pub specials_folder_format: String,
    pub standard_episode_format: String,
    pub daily_episode_format: String,
    pub anime_episode_format: String,
    pub multi_episode_style: MultiEpisodeStyle,
    pub max_file_name_bytes: usize,
    pub max_path_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NamingErrorDto {
    /// Field name, e.g. `movie_file_format`.
    pub field: String,
    pub message: String,
}

/// Example names rendered with sample data (`GET /naming/preview`, `POST /naming/preview`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NamingPreviewDto {
    pub movie_folder: String,
    pub movie_file: String,
    pub series_folder: String,
    pub season_folder: String,
    pub standard_episode: String,
    pub multi_episode: String,
    pub daily_episode: String,
    pub anime_episode: String,
    /// Template problems (unknown tokens, unbalanced braces); empty when valid.
    pub errors: Vec<NamingErrorDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    /// Hardlink when possible, else copy (torrents keep seeding).
    #[default]
    HardlinkOrCopy,
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RescanAfterRefresh {
    #[default]
    Always,
    AfterManual,
    Never,
}

/// `GET/PUT /settings/mediamanagement`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct MediaManagementSettings {
    pub import_mode: ImportMode,
    /// Deleted/replaced files go here instead of being removed (empty = delete permanently).
    pub recycle_bin_path: Option<String>,
    /// Days to keep recycled files (0 = forever).
    pub recycle_bin_retention_days: u32,
    pub set_permissions: bool,
    /// Octal, e.g. `644`.
    pub file_mode: Option<String>,
    /// Octal, e.g. `755`.
    pub dir_mode: Option<String>,
    pub chown_uid: Option<u32>,
    pub chown_gid: Option<u32>,
    /// Refuse imports when a root folder has less free space than this.
    pub minimum_free_space_mb: u64,
    pub skip_free_space_check: bool,
    pub delete_empty_folders: bool,
    pub create_empty_folders: bool,
    /// Unmonitor movies/episodes whose files were deleted outside Reelix.
    pub unmonitor_deleted: bool,
    pub rescan_after_refresh: RescanAfterRefresh,
    pub import_extra_files: bool,
    /// Extra file extensions imported next to the video (e.g. `srt`, `nfo`).
    pub extra_file_extensions: Vec<String>,
    /// ffprobe binary (default `ffprobe` from PATH).
    pub ffprobe_path: String,
    /// Read media info with ffprobe during disk scans.
    pub analyze_video_files: bool,
}

impl Default for MediaManagementSettings {
    fn default() -> Self {
        Self {
            import_mode: ImportMode::HardlinkOrCopy,
            recycle_bin_path: None,
            recycle_bin_retention_days: 7,
            set_permissions: false,
            file_mode: Some("644".into()),
            dir_mode: Some("755".into()),
            chown_uid: None,
            chown_gid: None,
            minimum_free_space_mb: 100,
            skip_free_space_check: false,
            delete_empty_folders: false,
            create_empty_folders: false,
            unmonitor_deleted: false,
            rescan_after_refresh: RescanAfterRefresh::Always,
            import_extra_files: true,
            extra_file_extensions: vec![
                "srt".into(),
                "ass".into(),
                "sub".into(),
                "idx".into(),
                "nfo".into(),
            ],
            ffprobe_path: "ffprobe".into(),
            analyze_video_files: true,
        }
    }
}

// ======================================================================
// Statistics
// ======================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MovieStats {
    pub total: u64,
    pub monitored: u64,
    pub with_file: u64,
    /// Monitored, available, no file.
    pub missing: u64,
    pub size_on_disk: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeriesStats {
    pub total: u64,
    pub monitored: u64,
    pub continuing: u64,
    pub ended: u64,
    pub episodes: u64,
    pub episodes_with_file: u64,
    /// Monitored aired episodes without a file.
    pub missing_episodes: u64,
    pub size_on_disk: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RootFolderSpaceDto {
    pub id: i64,
    pub path: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub media_kind: MediaKind,
    pub free_space: Option<u64>,
    pub total_space: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RecentItemDto {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub added: DateTime<Utc>,
}

/// `GET /library/stats` (dashboard tiles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LibraryStats {
    pub movies: MovieStats,
    pub series: SeriesStats,
    pub root_folders: Vec<RootFolderSpaceDto>,
    /// Newest 10 movies/series.
    pub recently_added: Vec<RecentItemDto>,
    pub total_size_on_disk: u64,
}
