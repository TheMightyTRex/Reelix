//! Music (M9): artists, albums (MusicBrainz release groups), releases, tracks, track files,
//! metadata profiles, lookup, library import, renaming and music settings.
//!
//! All routes are under `/api/v1` and need `MANAGE_LIBRARY`.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::content::ContentSearchKind;
use crate::library::{ImagesDto, RatingsDto, SortDir};

pub use crate::library::{LibraryImportResult, Paged, RenamePreviewDto};
pub use reelix_parser::QualityModel;

fn yes() -> bool {
    true
}

/// MusicBrainz primary release-group type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlbumType {
    #[default]
    Album,
    Ep,
    Single,
    Broadcast,
    Other,
}

/// MusicBrainz secondary release-group type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SecondaryAlbumType {
    Compilation,
    Soundtrack,
    Spokenword,
    Interview,
    Audiobook,
    AudioDrama,
    Live,
    Remix,
    DjMix,
    Mixtape,
    Demo,
    FieldRecording,
}

/// MusicBrainz release status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    Official,
    Promotion,
    Bootleg,
    PseudoRelease,
}

/// Which new albums found on refresh get monitored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NewAlbumMonitor {
    #[default]
    All,
    None,
    /// Only albums released after the artist was added.
    New,
}

/// Which albums get monitored when an artist is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ArtistMonitor {
    #[default]
    All,
    Future,
    Missing,
    Existing,
    Latest,
    First,
    None,
    /// Only `selected_release_group_ids`.
    Selected,
}

/// Which release groups an artist refresh adds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MetadataProfileDto {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub primary_types: Vec<AlbumType>,
    /// Allowed secondary types.
    pub secondary_types: Vec<SecondaryAlbumType>,
    pub release_statuses: Vec<ReleaseStatus>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtistStatisticsDto {
    pub album_count: u32,
    pub track_count: u32,
    pub track_file_count: u32,
    pub size_on_disk: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtistDto {
    pub id: i64,
    pub musicbrainz_id: String,
    pub name: String,
    pub sort_name: String,
    pub disambiguation: Option<String>,
    pub artist_type: Option<String>,
    pub country: Option<String>,
    pub overview: Option<String>,
    pub genres: Vec<String>,
    pub images: ImagesDto,
    pub links: Vec<String>,
    pub monitored: bool,
    pub monitor_new_items: NewAlbumMonitor,
    pub quality_profile_id: i64,
    pub metadata_profile_id: i64,
    pub root_folder_id: i64,
    pub path: String,
    pub tags: Vec<i64>,
    pub statistics: ArtistStatisticsDto,
    pub added: DateTime<Utc>,
    pub last_refreshed: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlbumReleaseDto {
    pub id: i64,
    pub musicbrainz_release_id: String,
    pub title: String,
    pub status: Option<ReleaseStatus>,
    pub country: Option<String>,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
    pub format: Option<String>,
    pub disc_count: u32,
    pub track_count: u32,
    pub release_date: Option<NaiveDate>,
    pub disambiguation: Option<String>,
    /// The album's selected release.
    pub selected: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlbumStatisticsDto {
    pub track_count: u32,
    pub track_file_count: u32,
    pub size_on_disk: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlbumDto {
    pub id: i64,
    pub artist_id: i64,
    pub artist_name: String,
    pub musicbrainz_release_group_id: String,
    pub title: String,
    pub disambiguation: Option<String>,
    pub album_type: AlbumType,
    pub secondary_types: Vec<SecondaryAlbumType>,
    pub release_date: Option<NaiveDate>,
    pub genres: Vec<String>,
    pub images: ImagesDto,
    pub ratings: RatingsDto,
    pub monitored: bool,
    pub selected_release_id: Option<i64>,
    pub any_release_ok: bool,
    /// Filled on `GET /albums/{id}`.
    #[serde(default)]
    pub releases: Vec<AlbumReleaseDto>,
    pub statistics: AlbumStatisticsDto,
    pub added_by_request_id: Option<i64>,
    pub added: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TrackFileDto {
    pub id: i64,
    pub album_id: i64,
    /// Relative to the artist's path.
    pub relative_path: String,
    pub path: String,
    pub size: u64,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub quality: QualityModel,
    pub quality_name: String,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub duration_ms: Option<u64>,
    pub release_group: Option<String>,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    pub custom_format_ids: Vec<i64>,
    pub custom_formats: Vec<String>,
    pub custom_format_score: i32,
    pub cutoff_met: bool,
    pub date_added: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TrackDto {
    pub id: i64,
    pub album_id: i64,
    pub release_id: i64,
    pub musicbrainz_track_id: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub disc_number: u32,
    pub track_number: u32,
    pub absolute_number: Option<u32>,
    pub title: String,
    pub duration_ms: Option<u64>,
    pub artist_credit: Option<String>,
    pub track_file_id: Option<i64>,
    pub track_file: Option<TrackFileDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtistLookupDto {
    pub musicbrainz_id: String,
    pub name: String,
    pub sort_name: String,
    pub disambiguation: Option<String>,
    pub artist_type: Option<String>,
    pub country: Option<String>,
    pub image: Option<String>,
    /// Library artist id when already added.
    pub artist_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlbumLookupDto {
    pub musicbrainz_release_group_id: String,
    pub title: String,
    pub artist_name: String,
    pub artist_musicbrainz_id: String,
    pub album_type: AlbumType,
    pub secondary_types: Vec<SecondaryAlbumType>,
    pub year: Option<i32>,
    pub cover: Option<String>,
    /// Library album id when already added.
    pub album_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AddArtistRequest {
    pub musicbrainz_id: String,
    pub quality_profile_id: i64,
    pub metadata_profile_id: i64,
    pub root_folder_id: i64,
    #[serde(default = "yes")]
    pub monitored: bool,
    #[serde(default)]
    pub monitor: ArtistMonitor,
    #[serde(default)]
    pub monitor_new_items: NewAlbumMonitor,
    #[serde(default)]
    pub selected_release_group_ids: Vec<String>,
    #[serde(default)]
    pub tags: Vec<i64>,
    #[serde(default)]
    pub search_on_add: bool,
}

/// Adds one album; creates the artist (monitor None, new items None) when missing.
/// `None` ids = the first profile / root folder of each kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AddAlbumRequest {
    pub musicbrainz_release_group_id: String,
    pub quality_profile_id: Option<i64>,
    pub metadata_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub tags: Vec<i64>,
    #[serde(default)]
    pub search_on_add: bool,
    #[serde(default)]
    pub added_by_request_id: Option<i64>,
}

/// `PUT /artists/{id}`; absent fields are unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtistEdit {
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub monitor_new_items: Option<NewAlbumMonitor>,
    #[serde(default)]
    pub quality_profile_id: Option<i64>,
    #[serde(default)]
    pub metadata_profile_id: Option<i64>,
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
    /// Move existing files when `path` changes.
    #[serde(default)]
    pub move_files: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlbumEdit {
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub any_release_ok: Option<bool>,
}

/// `PUT /albums/monitor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlbumMonitorRequest {
    pub album_ids: Vec<i64>,
    pub monitored: bool,
}

/// `PUT /albums/{id}/release`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChooseReleaseRequest {
    pub release_id: i64,
}

/// `GET /artists` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ArtistListQuery {
    pub search: Option<String>,
    pub monitored: Option<bool>,
    pub sort: Option<String>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub dir: Option<SortDir>,
    pub page: Option<u32>,
    /// Omit (or 0) for all items.
    pub page_size: Option<u32>,
}

/// `GET /albums` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct AlbumListQuery {
    pub artist_id: Option<i64>,
    pub search: Option<String>,
    pub monitored: Option<bool>,
    /// Monitored albums with missing tracks.
    pub missing: Option<bool>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

/// `GET /albums/wanted` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct AlbumWantedQuery {
    /// `missing` (default) or `cutoff`.
    pub kind: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

/// `POST /music/search` → 202; job kind `album_search` (handled by the pipeline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlbumSearchRequest {
    pub kind: ContentSearchKind,
    #[serde(default)]
    pub ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct MusicNamingDto {
    pub rename: bool,
    pub artist_folder: String,
    pub album_folder: String,
    pub track_file: String,
    pub multi_disc_track_file: String,
    /// Sub-folder per disc (e.g. `CD {medium}`); `None` = all discs in the album folder.
    pub multi_disc_folder: Option<String>,
}

impl Default for MusicNamingDto {
    fn default() -> Self {
        Self {
            rename: true,
            artist_folder: "{Artist Name}".into(),
            album_folder: "{Album Title} ({Release Year})".into(),
            track_file: "{track:00} - {Track Title}".into(),
            multi_disc_track_file: "{medium:00}-{track:00} - {Track Title}".into(),
            multi_disc_folder: None,
        }
    }
}

/// `GET/PUT /settings/music`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct MusicSettingsDto {
    pub naming: MusicNamingDto,
    /// Write tags into imported files (never into seeding files).
    pub write_tags: bool,
    pub embed_cover: bool,
    pub save_cover_file: bool,
    pub preferred_release_countries: Vec<String>,
    pub preferred_formats: Vec<String>,
}

impl Default for MusicSettingsDto {
    fn default() -> Self {
        Self {
            naming: MusicNamingDto::default(),
            write_tags: true,
            embed_cover: true,
            save_cover_file: true,
            preferred_release_countries: Vec::new(),
            preferred_formats: Vec::new(),
        }
    }
}

/// How well a release matches the files of a download (lower distance is better).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseMatchDto {
    pub release_id: i64,
    pub title: String,
    pub track_count: u32,
    pub distance: f32,
    pub missing_tracks: u32,
    pub extra_files: u32,
}

/// A folder under a music root that is not yet in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MusicImportCandidateDto {
    pub folder: String,
    pub path: String,
    pub guessed_artist: Option<String>,
    pub suggestions: Vec<ArtistLookupDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MusicImportItem {
    pub path: String,
    pub musicbrainz_id: String,
    pub quality_profile_id: i64,
    pub metadata_profile_id: i64,
    #[serde(default)]
    pub monitor: ArtistMonitor,
    #[serde(default)]
    pub tags: Vec<i64>,
}

/// `POST /musicimport`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MusicLibraryImportRequest {
    pub root_folder_id: i64,
    pub items: Vec<MusicImportItem>,
}

/// `GET/POST /music/rename?artist_id=`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct MusicRenameQuery {
    pub artist_id: i64,
}

/// `GET /tracks?album_id=` / `GET /trackfiles?album_id=`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct AlbumIdQuery {
    pub album_id: i64,
}

/// `GET /musicimport/scan?root_folder_id=`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct MusicImportScanQuery {
    pub root_folder_id: i64,
}

/// `POST /artists/refresh` and `POST /artists/rescan` (empty `artist_ids` = every artist).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtistJobRequest {
    #[serde(default)]
    pub artist_ids: Vec<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_use_snake_case() {
        assert_eq!(
            serde_json::to_string(&ReleaseStatus::PseudoRelease).unwrap(),
            "\"pseudo_release\""
        );
        assert_eq!(
            serde_json::to_string(&SecondaryAlbumType::DjMix).unwrap(),
            "\"dj_mix\""
        );
        let s: MusicSettingsDto = serde_json::from_str("{}").unwrap();
        assert_eq!(s, MusicSettingsDto::default());
    }
}
