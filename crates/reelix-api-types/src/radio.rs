//! Radio: shows (radio programmes and podcasts) with their series and episodes, stations,
//! provider search and preview, the Audiobookshelf podcast library review and radio settings.
//!
//! All routes are under `/api/v1/radio` and need `MANAGE_LIBRARY`; settings need `ADMIN`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::library::SortDir;

pub use crate::library::Paged;

fn yes() -> bool {
    true
}

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
fn double<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// Which episodes of a show are wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RadioFollow {
    /// Every episode, old and new.
    #[default]
    All,
    /// Only episodes first broadcast at or after `followed_at`.
    FromNowOn,
    /// Keep the list only.
    None,
}

/// A real series, or the specials (trailers, bonus episodes; filed as series 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RadioSeriesKind {
    #[default]
    Series,
    Specials,
}

/// Where a series or episode number came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RadioNumberSource {
    /// The provider gave it.
    #[default]
    Source,
    /// Reelix numbered the episodes of the series by first broadcast.
    BroadcastOrder,
}

/// Episode status, with the same words as books: In library, Wanted, Not released, Not
/// requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RadioEpisodeStatus {
    /// A file in the podcast library is matched to it.
    InLibrary,
    /// Monitored, broadcast, no file.
    Wanted,
    /// Monitored, not broadcast yet.
    NotReleased,
    /// Not monitored (follow choice `none`, before `followed_at`, or the show is unmonitored).
    #[default]
    NotRequested,
}

/// A broadcaster's service or a podcast network.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioStationDto {
    pub id: i64,
    pub provider: String,
    pub external_id: String,
    pub name: String,
    pub logo_url: Option<String>,
    /// A logo was downloaded and is served by Reelix.
    pub has_logo: bool,
    pub show_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioShowStatisticsDto {
    pub series_count: u32,
    pub episode_count: u32,
    pub in_library_count: u32,
    pub wanted_count: u32,
}

/// A followed radio show or podcast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioShowDto {
    pub id: i64,
    /// Provider id (`rss`, `bbc`, …).
    pub provider: String,
    /// The show's id at the provider (the feed URL for `rss`).
    pub external_id: String,
    pub title: String,
    pub sort_title: String,
    pub synopsis: Option<String>,
    pub author: Option<String>,
    pub web_url: Option<String>,
    pub first_broadcast: Option<DateTime<Utc>>,
    pub station_id: Option<i64>,
    pub station_name: Option<String>,
    pub artwork_url: Option<String>,
    /// Artwork was downloaded and is served by Reelix.
    pub has_artwork: bool,
    pub feed_url: Option<String>,
    pub follow: RadioFollow,
    pub followed_at: Option<DateTime<Utc>>,
    pub monitored: bool,
    pub tags: Vec<i64>,
    pub root_folder_id: Option<i64>,
    /// The Audiobookshelf podcast item this show maps to.
    pub abs_item_id: Option<String>,
    pub quality_profile_id: Option<i64>,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub added_at: DateTime<Utc>,
    pub statistics: RadioShowStatisticsDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioSeriesDto {
    pub id: i64,
    pub show_id: i64,
    pub external_id: String,
    pub kind: RadioSeriesKind,
    pub number: Option<u32>,
    pub number_source: RadioNumberSource,
    pub title: Option<String>,
    pub synopsis: Option<String>,
    pub expected_episodes: Option<u32>,
    pub episode_count: u32,
    /// Complete and quiet for 60 days; refresh no longer reads it.
    pub settled_at: Option<DateTime<Utc>>,
    /// The provider stopped listing it (kept, never deleted).
    pub source_removed_at: Option<DateTime<Utc>>,
}

/// One episode file of the Audiobookshelf podcast library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioEpisodeFileDto {
    pub id: i64,
    pub abs_item_id: String,
    pub abs_episode_id: String,
    /// The podcast's title in Audiobookshelf.
    pub item_title: Option<String>,
    pub title: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub guid: Option<String>,
    pub enclosure_url: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<u32>,
    pub path: Option<String>,
    pub size: Option<u64>,
    pub seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioEpisodeDto {
    pub id: i64,
    pub show_id: i64,
    pub series_id: i64,
    pub external_id: String,
    /// Series number (0 for specials when filed); copied from the series for display.
    pub series_number: Option<u32>,
    pub series_kind: RadioSeriesKind,
    pub number: Option<u32>,
    pub number_source: RadioNumberSource,
    pub title: String,
    pub synopsis: Option<String>,
    pub first_broadcast: Option<DateTime<Utc>>,
    pub duration_secs: Option<u32>,
    pub monitored: bool,
    pub status: RadioEpisodeStatus,
    pub file_id: Option<i64>,
    /// The file link was set by hand and is kept by automatic matching.
    pub manual_match: bool,
    pub enclosure_url: Option<String>,
    pub guid: Option<String>,
    pub source_removed_at: Option<DateTime<Utc>>,
    /// The matched file (single-episode reads only; `None` in lists).
    #[serde(default)]
    pub file: Option<RadioEpisodeFileDto>,
}

/// `GET /radio/shows/{id}/episodes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioShowEpisodesDto {
    pub series: Vec<RadioSeriesDto>,
    /// Ordered by series, then number, then first broadcast.
    pub episodes: Vec<RadioEpisodeDto>,
}

/// A show found by a provider search (nothing saved).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioShowHitDto {
    pub provider: String,
    pub external_id: String,
    pub title: String,
    pub synopsis: Option<String>,
    pub author: Option<String>,
    pub station_name: Option<String>,
    pub artwork_url: Option<String>,
    pub web_url: Option<String>,
    pub feed_url: Option<String>,
    pub episode_count: Option<u32>,
    /// Library show id when already added.
    #[serde(default)]
    pub show_id: Option<i64>,
}

/// `GET /radio/preview?link=`: the show behind a pasted link, with a first look at its
/// episodes (nothing saved).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioShowPreviewDto {
    pub show: RadioShowHitDto,
    pub first_broadcast: Option<DateTime<Utc>>,
    pub series_count: u32,
    pub episode_count: u32,
    /// The newest episodes (at most 10).
    pub latest_episodes: Vec<RadioPreviewEpisodeDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioPreviewEpisodeDto {
    pub title: String,
    pub series_number: Option<u32>,
    pub number: Option<u32>,
    pub first_broadcast: Option<DateTime<Utc>>,
    pub duration_secs: Option<u32>,
}

/// One provider as listed by `GET /radio/providers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioProviderDto {
    pub id: String,
    pub name: String,
    /// Search by name is supported (otherwise only pasted links).
    pub supports_search: bool,
    pub enabled: bool,
}

/// `GET /radio/shows` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct RadioShowListQuery {
    pub search: Option<String>,
    pub station_id: Option<i64>,
    /// `true` = follow `all` or `from_now_on`; `false` = follow `none`.
    pub followed: Option<bool>,
    pub monitored: Option<bool>,
    /// `sort_title` (default), `title`, `added`, `refreshed`, `first_broadcast`.
    pub sort: Option<String>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub dir: Option<SortDir>,
    pub page: Option<u32>,
    /// Omit (or 0) for all items.
    pub page_size: Option<u32>,
}

/// `GET /radio/search` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct RadioSearchQuery {
    pub q: String,
    /// One provider id; omitted = every enabled provider that can search.
    pub provider: Option<String>,
}

/// `GET /radio/preview` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct RadioPreviewQuery {
    /// A show page or feed URL.
    pub link: String,
}

/// `POST /radio/shows`: either `provider` + `external_id` (from a search hit) or `link`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AddRadioShowRequest {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
    /// Defaults to the settings' follow choice.
    #[serde(default)]
    pub follow: Option<RadioFollow>,
    #[serde(default = "yes")]
    pub monitored: bool,
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub quality_profile_id: Option<i64>,
    #[serde(default)]
    pub tags: Vec<i64>,
}

/// `PUT /radio/shows/{id}` (partial; `null` clears the nullable fields).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioShowEdit {
    /// Changing it re-applies the choice to every episode without a hand-set state;
    /// `from_now_on` restarts `followed_at`.
    #[serde(default)]
    pub follow: Option<RadioFollow>,
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<i64>))]
    pub root_folder_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<i64>))]
    pub quality_profile_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub abs_item_id: Option<Option<String>>,
}

/// `PUT /radio/episodes/{id}/file`: links a library file to the episode by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioEpisodeFileRequest {
    pub file_id: i64,
}

/// `DELETE /radio/episodes/{id}/file` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct RadioClearFileQuery {
    /// `false` (default, "Not this file"): keep the episode unmatched until set by hand;
    /// `true` ("Match automatically again"): the next library sync may match it.
    #[serde(default)]
    pub auto: bool,
}

/// `GET /radio/library/review`: what the podcast library has that no followed show covers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioLibraryReviewDto {
    /// Podcast items in the library that no show maps to.
    pub unmatched_shows: Vec<RadioLibraryShowDto>,
    /// Episode files matched to no episode.
    pub unmatched_files: Vec<RadioEpisodeFileDto>,
    /// Last `radio_library_sync` (none yet = the library was never read).
    pub synced_at: Option<DateTime<Utc>>,
}

/// One podcast item of the Audiobookshelf library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioLibraryShowDto {
    pub abs_item_id: String,
    pub title: Option<String>,
    pub file_count: u32,
}

/// `GET/PUT /radio/settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RadioSettingsDto {
    /// Audiobookshelf podcast library id.
    pub abs_library_id: Option<String>,
    /// The library folder as Audiobookshelf sees it (inside its container).
    pub abs_library_folder: Option<String>,
    /// The same folder as Reelix sees it; must exist and be writable.
    pub library_folder: Option<String>,
    /// Follow choice for newly added shows.
    pub default_follow: RadioFollow,
    /// Provider ids that may be used; empty = all.
    pub enabled_providers: Vec<String>,
    /// ISO 3166 country code preferred by searches (`GB`, `US`).
    pub country: Option<String>,
    /// ISO 639 language code preferred by searches (`en`).
    pub language: Option<String>,
    /// Expensive per-episode lookups (lengths, details) a refresh may make per show.
    pub max_lookups_per_refresh: u32,
}

impl Default for RadioSettingsDto {
    fn default() -> Self {
        Self {
            abs_library_id: None,
            abs_library_folder: None,
            library_folder: None,
            default_follow: RadioFollow::All,
            enabled_providers: Vec::new(),
            country: None,
            language: None,
            max_lookups_per_refresh: 30,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names() {
        assert_eq!(
            serde_json::to_string(&RadioFollow::FromNowOn).unwrap(),
            "\"from_now_on\""
        );
        assert_eq!(
            serde_json::to_string(&RadioEpisodeStatus::NotRequested).unwrap(),
            "\"not_requested\""
        );
        assert_eq!(
            serde_json::to_string(&RadioNumberSource::BroadcastOrder).unwrap(),
            "\"broadcast_order\""
        );
        let s: RadioSettingsDto = serde_json::from_str("{}").unwrap();
        assert_eq!(s, RadioSettingsDto::default());
        let e: RadioShowEdit = serde_json::from_str(r#"{"root_folder_id": null}"#).unwrap();
        assert_eq!(e.root_folder_id, Some(None));
        assert_eq!(e.quality_profile_id, None);
        let a: AddRadioShowRequest = serde_json::from_str(r#"{"link": "https://x/feed"}"#).unwrap();
        assert!(a.monitored);
    }
}
