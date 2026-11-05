//! DTOs for requests, discovery, issues, user management and external login (M6).
//!
//! Routes (all under `/api/v1`):
//! - discovery: `GET /discover/{feed}`, `GET /discover/genres/{kind}`, `GET /search?q=&page=`,
//!   `GET /media/movie/{tmdbId}`, `GET /media/tv/{tmdbId}` (+ `/recommendations`, `/similar`)
//! - requests: `GET/POST /requests`, `GET/PUT/DELETE /requests/{id}`,
//!   `POST /requests/{id}/approve|decline|retry`, `GET /requests/count`, `GET /requests/options`,
//!   `GET /users/{id}/quota`
//! - issues: `GET/POST /issues`, `GET/PUT/DELETE /issues/{id}`, `POST /issues/{id}/comments`,
//!   `POST /issues/{id}/resolve|reopen`, `GET /issues/count`
//! - users: `GET/POST /users`, `GET/PUT/DELETE /users/{id}`, `GET /users/roles`,
//!   `GET /users/import/servers`, `GET /users/import/{media_server_id}` (preview),
//!   `POST /users/import/{media_server_id}`
//! - settings: `GET/PUT /settings/requests`, `GET/PUT /settings/security`
//! - auth: `GET /auth/methods`, `POST /auth/jellyfin`, `POST /auth/emby`, `POST /auth/plex/pin`,
//!   `POST /auth/plex/pin/{id}`, `GET /auth/oidc/login?redirect=`, `GET /auth/oidc/callback`
//! - profile: `GET/PUT /profile/notifications`
//! - book/music requests (M8/M9): `GET/POST /requests/media`, `GET /requests/media/options`,
//!   `GET/PUT/DELETE /requests/media/{id}`, `POST /requests/media/{id}/approve|decline|retry`
//! - book/music discovery: `GET /search/books?q=`, `GET /search/music?q=`,
//!   `GET /media/book/{workId}`, `GET /media/album/{releaseGroupId}`
//! - radio discovery: `GET /discover/genres/radio`, `GET /discover/content/radio/{row}`,
//!   `GET /search/radio?q=`, `GET /media/radio/{provider}/{externalId}`

use chrono::{DateTime, NaiveDate, Utc};
use reelix_core::{BookFormat, MediaKind, Permissions};
use serde::{Deserialize, Serialize};

use crate::UserDto;

// ---------------------------------------------------------------- shared

/// Library availability of a movie, series or season.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityStatus {
    /// Not in the library.
    #[default]
    Unknown,
    /// In the library and monitored, nothing downloaded yet.
    Monitored,
    /// A download is in progress / the pipeline is working on it.
    Processing,
    /// Some (not all) requested/aired episodes are on disk.
    PartiallyAvailable,
    /// Fully on disk.
    Available,
}

/// Lifecycle of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Approved,
    Declined,
    Processing,
    PartiallyAvailable,
    Available,
    /// Approval failed (e.g. adding to the library failed); see `failure_reason`. Can be retried.
    Failed,
}

impl RequestStatus {
    pub const ALL: [RequestStatus; 7] = [
        Self::Pending,
        Self::Approved,
        Self::Declined,
        Self::Processing,
        Self::PartiallyAvailable,
        Self::Available,
        Self::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Declined => "declined",
            Self::Processing => "processing",
            Self::PartiallyAvailable => "partially_available",
            Self::Available => "available",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Pending, approved or in progress (blocks duplicate requests).
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Declined | Self::Failed)
    }
}

/// Minimal user reference shown next to requests, issues and comments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UserBriefDto {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub avatar: Option<String>,
}

/// `{ id, name }` pair (quality profiles, genres, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NamedIdDto {
    pub id: i64,
    pub name: String,
}

/// Compact request state overlaid on discover/search items and media details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RequestBriefDto {
    pub id: i64,
    pub status: RequestStatus,
    pub is_4k: bool,
    /// Requested seasons (series only; empty for movies).
    pub seasons: Vec<u32>,
    pub requested_by: UserBriefDto,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------- discovery

/// One poster card in a discover row or search results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscoverItemDto {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub release_date: Option<NaiveDate>,
    pub overview: Option<String>,
    /// Absolute poster URL (image.tmdb.org).
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub genre_ids: Vec<u32>,
    pub original_language: Option<String>,
    pub vote_average: f64,
    pub vote_count: u32,
    pub popularity: f64,
    pub availability: AvailabilityStatus,
    /// The most relevant non-4K request (active first), if any.
    pub request: Option<RequestBriefDto>,
    /// The most relevant 4K request, if any.
    pub request_4k: Option<RequestBriefDto>,
}

/// A paged discover feed or search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscoverPageDto {
    pub page: u32,
    pub total_pages: u32,
    pub total_results: u32,
    pub results: Vec<DiscoverItemDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PersonDto {
    pub tmdb_id: u32,
    pub name: String,
    pub known_for_department: Option<String>,
    pub profile_url: Option<String>,
}

/// `GET /search?q=&page=`: movies/series (with availability) plus matching people.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchPageDto {
    pub page: u32,
    pub total_pages: u32,
    pub total_results: u32,
    pub results: Vec<DiscoverItemDto>,
    pub people: Vec<PersonDto>,
}

/// Query for `/discover/{feed}`, `/search` and `/media/*/recommendations`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams, utoipa::ToSchema))]
pub struct PageQuery {
    /// Search text (`/search` only).
    #[serde(default)]
    pub q: Option<String>,
    /// 1-based page, default 1.
    #[serde(default)]
    pub page: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GenreDto {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CompanyDto {
    pub id: u32,
    pub name: String,
    pub logo_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CastDto {
    pub tmdb_id: u32,
    pub name: String,
    pub character: Option<String>,
    pub profile_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CrewDto {
    pub tmdb_id: u32,
    pub name: String,
    pub job: String,
    pub profile_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VideoDto {
    pub name: String,
    /// "Trailer", "Teaser", …
    pub kind: String,
    pub site: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CollectionRefDto {
    pub tmdb_id: u32,
    pub name: String,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
}

/// One season on the TV detail page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SeasonDto {
    pub season_number: u32,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<NaiveDate>,
    pub poster: Option<String>,
    pub episode_count: u32,
    pub availability: AvailabilityStatus,
    /// Status of the active non-4K request covering this season, if any.
    pub request_status: Option<RequestStatus>,
    /// Status of the active 4K request covering this season, if any.
    pub request_status_4k: Option<RequestStatus>,
}

/// What the current user may do on a media detail page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RequestAbilityDto {
    /// Has REQUEST_MOVIE / REQUEST_TV and there is something left to request.
    pub can_request: bool,
    /// 4K is enabled for this kind and the user has REQUEST_4K.
    pub can_request_4k: bool,
    /// May pick profile / root folder (REQUEST_ADVANCED).
    pub can_request_advanced: bool,
    /// Individual seasons may be requested (else the whole series is requested).
    pub partial_seasons: bool,
    /// Why `can_request` is false (e.g. "already available", "quota exceeded").
    pub reason: Option<String>,
}

/// `GET /media/movie/{tmdbId}` and `GET /media/tv/{tmdbId}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MediaDetailDto {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub original_language: Option<String>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub year: Option<i32>,
    /// Movie release date / series first air date.
    pub release_date: Option<NaiveDate>,
    /// Series only.
    pub last_air_date: Option<NaiveDate>,
    /// Series only.
    pub next_air_date: Option<NaiveDate>,
    /// Movies: cinema/digital/physical dates for the configured region.
    pub cinema_date: Option<NaiveDate>,
    pub digital_date: Option<NaiveDate>,
    pub physical_date: Option<NaiveDate>,
    /// Provider status text, e.g. `released`, `continuing`, `ended`.
    pub status: String,
    pub runtime_minutes: Option<u32>,
    pub certification: Option<String>,
    pub genres: Vec<GenreDto>,
    /// Studios (movies) or networks (series).
    pub companies: Vec<CompanyDto>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub logo: Option<String>,
    pub cast: Vec<CastDto>,
    /// Directors, writers, creators, producers (trimmed).
    pub crew: Vec<CrewDto>,
    pub videos: Vec<VideoDto>,
    /// First official YouTube trailer, if any.
    pub trailer_url: Option<String>,
    pub vote_average: f64,
    pub vote_count: u32,
    pub homepage: Option<String>,
    pub collection: Option<CollectionRefDto>,
    /// Series only (specials excluded).
    pub seasons: Vec<SeasonDto>,
    pub availability: AvailabilityStatus,
    /// Library id when the title is in the library.
    pub library_id: Option<i64>,
    /// Every request for this title visible to the caller (own, or all with VIEW_REQUESTS).
    pub requests: Vec<RequestBriefDto>,
    pub request_ability: RequestAbilityDto,
    /// First page of recommendations (with availability).
    pub recommendations: Vec<DiscoverItemDto>,
    /// First page of similar titles (with availability).
    pub similar: Vec<DiscoverItemDto>,
}

// ---------------------------------------------------------------- requests

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RequestDto {
    pub id: i64,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    /// Requested seasons (series only).
    pub seasons: Vec<u32>,
    pub status: RequestStatus,
    pub is_4k: bool,
    /// Override chosen by an advanced requester / admin (`None` = default for the kind).
    pub profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
    pub requested_by: UserBriefDto,
    /// Who approved / declined / last edited it.
    pub modified_by: Option<UserBriefDto>,
    pub decline_reason: Option<String>,
    pub failure_reason: Option<String>,
    /// Library ids once approved.
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateRequestDto {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: u32,
    /// Series: seasons to request; `None` = every (regular) season. Ignored for movies.
    #[serde(default)]
    pub seasons: Option<Vec<u32>>,
    #[serde(default)]
    pub is_4k: bool,
    /// Advanced options (REQUEST_ADVANCED).
    #[serde(default)]
    pub profile_id: Option<i64>,
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub tags: Vec<i64>,
    /// Request on behalf of another user (MANAGE_REQUESTS).
    #[serde(default)]
    pub user_id: Option<i64>,
}

/// `PUT /requests/{id}`: owner (while pending) or MANAGE_REQUESTS. `None` = unchanged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateRequestDto {
    #[serde(default)]
    pub seasons: Option<Vec<u32>>,
    #[serde(default)]
    pub is_4k: Option<bool>,
    #[serde(default)]
    pub profile_id: Option<i64>,
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeclineRequestDto {
    #[serde(default)]
    pub reason: Option<String>,
}

/// Query for `GET /requests`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams, utoipa::ToSchema))]
pub struct RequestListQuery {
    /// `all` (default), `pending`, `approved`, `declined`, `processing`,
    /// `partially_available`, `available`, `failed`, or `active` (pending+approved+processing).
    #[serde(default)]
    pub status: Option<String>,
    /// `movie` or `series`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Filter by requester (only effective with VIEW_REQUESTS).
    #[serde(default)]
    pub user_id: Option<i64>,
    /// `created` (default), `updated` or `title`.
    #[serde(default)]
    pub sort: Option<String>,
    /// `desc` (default) or `asc`.
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub page: Option<u32>,
    /// Default 25, max 100.
    #[serde(default)]
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RequestPageDto {
    pub items: Vec<RequestDto>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
}

/// `GET /requests/count` (scoped like the list).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RequestCountsDto {
    pub total: u64,
    pub pending: u64,
    pub approved: u64,
    pub declined: u64,
    pub processing: u64,
    pub partially_available: u64,
    pub available: u64,
    pub failed: u64,
    pub movie: u64,
    pub series: u64,
    /// Book / album requests (included in the status counts; omitted when zero).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub book: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub music: u64,
    /// Radio show requests (omitted when zero).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub radio: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RootFolderOptionDto {
    pub id: i64,
    pub path: String,
    pub free_space: Option<u64>,
}

/// `GET /requests/options?kind=movie|series&is_4k=`: choices for the advanced request modal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RequestOptionsDto {
    pub profiles: Vec<NamedIdDto>,
    pub root_folders: Vec<RootFolderOptionDto>,
    pub default_profile_id: Option<i64>,
    pub default_root_folder_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams, utoipa::ToSchema))]
pub struct RequestOptionsQuery {
    /// `movie` (default) or `series`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub is_4k: Option<bool>,
}

/// Usage of one quota (movies count requests; TV counts requested seasons).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QuotaUsageDto {
    /// `None` = unlimited.
    pub limit: Option<u32>,
    /// Rolling window length in days.
    pub days: u32,
    pub used: u32,
    pub remaining: Option<u32>,
    /// True when no further requests are allowed.
    pub restricted: bool,
}

/// `GET /users/{id}/quota`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QuotaDto {
    pub movie: QuotaUsageDto,
    pub tv: QuotaUsageDto,
    /// Book requests (each request counts once, whatever its formats).
    #[serde(default)]
    pub book: QuotaUsageDto,
    /// Album requests.
    #[serde(default)]
    pub music: QuotaUsageDto,
    /// Radio show requests.
    #[serde(default)]
    pub radio: QuotaUsageDto,
    /// Users with MANAGE_REQUESTS bypass quotas.
    pub exempt: bool,
}

// ---------------------------------------------------------------- book / music requests

/// Kind of a book/music/radio request (`book` = an Open Library work, `music` = a MusicBrainz
/// release group, i.e. an album, `radio` = a radio programme or podcast to follow).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ContentRequestKind {
    Book,
    Music,
    Radio,
}

impl ContentRequestKind {
    pub const ALL: [ContentRequestKind; 3] = [Self::Book, Self::Music, Self::Radio];

    /// `book` | `music` | `radio` (the `requests.media_kind` value).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Book => "book",
            Self::Music => "music",
            Self::Radio => "radio",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "book" | "books" => Some(Self::Book),
            "music" | "album" | "albums" => Some(Self::Music),
            "radio" | "podcast" | "podcasts" => Some(Self::Radio),
            _ => None,
        }
    }
}

/// The foreign id of a radio request or Discover item: `{provider}:{external_id}` (the
/// external id may itself contain `:`, e.g. a feed URL).
pub fn radio_foreign_id(provider: &str, external_id: &str) -> String {
    format!("{provider}:{external_id}")
}

/// `(provider, external_id)` of a radio foreign id ([`radio_foreign_id`]).
pub fn split_radio_foreign_id(id: &str) -> Option<(&str, &str)> {
    let (p, e) = id.split_once(':')?;
    let provider_ok = !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    (provider_ok && !e.trim().is_empty()).then_some((p, e))
}

/// A book or album request (`GET /requests/media`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentRequestDto {
    pub id: i64,
    pub kind: ContentRequestKind,
    /// Open Library work id (book), MusicBrainz release-group id (album) or
    /// `{provider}:{external_id}` (radio).
    pub foreign_id: String,
    pub title: String,
    /// Author (book), artist (album) or station / author (radio) name.
    pub subtitle: Option<String>,
    pub year: Option<i32>,
    /// Cover URL.
    pub poster: Option<String>,
    /// Requested book formats (books only; empty for albums).
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub formats: Vec<BookFormat>,
    pub status: RequestStatus,
    pub profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
    pub requested_by: UserBriefDto,
    pub modified_by: Option<UserBriefDto>,
    pub decline_reason: Option<String>,
    pub failure_reason: Option<String>,
    /// Library ids once approved.
    pub book_id: Option<i64>,
    pub author_id: Option<i64>,
    pub album_id: Option<i64>,
    pub artist_id: Option<i64>,
    /// Radio: the followed show once approved.
    #[serde(default)]
    pub radio_show_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
}

/// `POST /requests/media`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateContentRequestDto {
    pub kind: ContentRequestKind,
    /// Open Library work id (`OL…W`), MusicBrainz release-group id or, for radio,
    /// `{provider}:{external_id}` of a Discover / search item.
    pub foreign_id: String,
    /// Books: formats to request; empty = the configured default formats (or both).
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub formats: Vec<BookFormat>,
    /// Advanced options (REQUEST_ADVANCED). For books they apply to the single requested
    /// format (they are rejected when both formats are requested). Radio: an audiobook
    /// quality profile and a radio root folder.
    #[serde(default)]
    pub profile_id: Option<i64>,
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub tags: Vec<i64>,
    /// Request on behalf of another user (MANAGE_REQUESTS).
    #[serde(default)]
    pub user_id: Option<i64>,
}

/// `PUT /requests/media/{id}`: owner (while pending) or MANAGE_REQUESTS. `None` = unchanged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateContentRequestDto {
    /// Books only.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Vec<String>>))]
    pub formats: Option<Vec<BookFormat>>,
    #[serde(default)]
    pub profile_id: Option<i64>,
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentRequestPageDto {
    pub items: Vec<ContentRequestDto>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
}

/// `GET /requests/media/options?kind=ebook|audiobook|music|radio`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams, utoipa::ToSchema))]
pub struct ContentRequestOptionsQuery {
    /// `ebook` (default), `audiobook`, `music` or `radio`.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Compact book/album request state overlaid on search results and detail views.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentRequestBriefDto {
    pub id: i64,
    pub status: RequestStatus,
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub formats: Vec<BookFormat>,
    pub requested_by: UserBriefDto,
    pub created_at: DateTime<Utc>,
}

/// One book, album or radio show in search results and Discover rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentItemDto {
    pub kind: ContentRequestKind,
    /// Open Library work id, MusicBrainz release-group id or `{provider}:{external_id}`
    /// (radio).
    pub foreign_id: String,
    pub title: String,
    /// Author / artist name; radio: the station, else the author.
    pub subtitle: Option<String>,
    /// Open Library author id / MusicBrainz artist id.
    pub subtitle_id: Option<String>,
    pub year: Option<i32>,
    pub cover: Option<String>,
    /// Albums: `Album`, `EP`, `Single`, … (`None` for books).
    pub album_type: Option<String>,
    /// Radio: `available` = the show is followed (in the radio library).
    pub availability: AvailabilityStatus,
    /// Library book / album id, or radio show id, when in the library.
    pub library_id: Option<i64>,
    /// The most relevant request visible to the caller (active first), if any.
    pub request: Option<ContentRequestBriefDto>,
}

/// An artist in music search results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArtistItemDto {
    pub musicbrainz_id: String,
    pub name: String,
    pub disambiguation: Option<String>,
    pub artist_type: Option<String>,
    pub country: Option<String>,
    /// Library artist id when the artist is in the library.
    pub library_id: Option<i64>,
}

/// `GET /search/books?q=`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookSearchPageDto {
    pub results: Vec<ContentItemDto>,
}

/// `GET /search/music?q=`: matching albums and artists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MusicSearchPageDto {
    pub albums: Vec<ContentItemDto>,
    pub artists: Vec<ArtistItemDto>,
}

/// `GET /search/radio?q=`: radio shows from the enabled radio sources (a pasted link is
/// previewed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioSearchPageDto {
    pub results: Vec<ContentItemDto>,
}

/// A radio Discover list: `id` is the row id of `GET /discover/content/radio/{row}`
/// (`{provider}-{list}`, e.g. `apple-top`, `bbc-category-comedy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioDiscoverListDto {
    pub id: String,
    pub name: String,
    /// Radio source id (`apple`, `bbc`, `podcastindex`) and its display name.
    pub provider: String,
    pub provider_name: String,
}

/// `GET /discover/genres/radio`: the radio Discover lists of the enabled sources for the
/// radio settings' country and language: rows (charts, trending) and categories.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RadioDiscoverDto {
    pub rows: Vec<RadioDiscoverListDto>,
    pub categories: Vec<RadioDiscoverListDto>,
}

/// Discover rows for books: `(row id, title)`. Subject rows use Open Library subject keys.
pub const BOOK_DISCOVER_ROWS: &[(&str, &str)] = &[
    ("trending", "Trending this week"),
    ("subject-fantasy", "Fantasy"),
    ("subject-science_fiction", "Science fiction"),
    ("subject-mystery", "Mystery"),
    ("subject-thriller", "Thriller"),
    ("subject-romance", "Romance"),
    ("subject-horror", "Horror"),
    ("subject-historical_fiction", "Historical fiction"),
    ("subject-young_adult_fiction", "Young adult"),
    ("subject-biography", "Biography"),
];

/// Discover rows for music: `(row id, title)`.
pub const MUSIC_DISCOVER_ROWS: &[(&str, &str)] = &[
    ("new-releases", "New releases"),
    ("popular-week", "Popular this week"),
    ("popular-month", "Popular this month"),
];

impl ContentRequestKind {
    /// The fixed Discover rows of this kind: `(row id, title)`. Radio rows depend on the
    /// enabled sources and are listed by `GET /discover/genres/radio` instead (empty here).
    pub fn discover_rows(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Book => BOOK_DISCOVER_ROWS,
            Self::Music => MUSIC_DISCOVER_ROWS,
            Self::Radio => &[],
        }
    }
}

/// `GET /discover/content/{kind}/{row}`: one Discover row of books or albums.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentRowDto {
    pub results: Vec<ContentItemDto>,
}

/// What the current user may do in a book/album request view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentRequestAbilityDto {
    /// Has REQUEST_BOOK / REQUEST_MUSIC / REQUEST_RADIO and there is something left to
    /// request.
    pub can_request: bool,
    pub can_request_advanced: bool,
    /// Books: formats that can still be requested (not covered by an active request).
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub formats: Vec<BookFormat>,
    /// Books: formats preselected in the dialog.
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub default_formats: Vec<BookFormat>,
    /// Why `can_request` is false.
    pub reason: Option<String>,
}

/// `GET /media/book/{workId}`, `GET /media/album/{releaseGroupId}` and
/// `GET /media/radio/{provider}/{externalId}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentDetailDto {
    #[serde(flatten)]
    pub item: ContentItemDto,
    pub overview: Option<String>,
    /// Every request for this book/album/show visible to the caller.
    pub requests: Vec<ContentRequestBriefDto>,
    pub request_ability: ContentRequestAbilityDto,
    /// Radio: the show as its source sees it now (station, links, newest episodes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio: Option<crate::radio::RadioShowPreviewDto>,
}

// ---------------------------------------------------------------- issues

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Video,
    Audio,
    Subtitles,
    Other,
}

impl IssueType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Subtitles => "subtitles",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Video, Self::Audio, Self::Subtitles, Self::Other]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Open,
    Resolved,
}

impl IssueStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IssueCommentDto {
    pub id: i64,
    pub user: UserBriefDto,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IssueDto {
    pub id: i64,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub issue_type: IssueType,
    pub status: IssueStatus,
    /// Series only; `None` = whole series / all seasons.
    pub season: Option<u32>,
    pub episode: Option<u32>,
    /// The initial description.
    pub message: String,
    pub reported_by: UserBriefDto,
    pub resolved_by: Option<UserBriefDto>,
    pub comment_count: u32,
    /// Thread, oldest first. Filled by `GET /issues/{id}` (and mutations); empty in lists.
    pub comments: Vec<IssueCommentDto>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateIssueDto {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub issue_type: IssueType,
    #[serde(default)]
    pub season: Option<u32>,
    #[serde(default)]
    pub episode: Option<u32>,
    pub message: String,
}

/// `PUT /issues/{id}` (reporter or MANAGE_ISSUES). `None` = unchanged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateIssueDto {
    #[serde(default)]
    pub issue_type: Option<IssueType>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub season: Option<u32>,
    #[serde(default)]
    pub episode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateCommentDto {
    pub message: String,
}

/// Query for `GET /issues`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams, utoipa::ToSchema))]
pub struct IssueListQuery {
    /// `all` (default), `open` or `resolved`.
    #[serde(default)]
    pub status: Option<String>,
    /// `video`, `audio`, `subtitles`, `other`.
    #[serde(default)]
    pub issue_type: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub tmdb_id: Option<u32>,
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IssuePageDto {
    pub items: Vec<IssueDto>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IssueCountsDto {
    pub total: u64,
    pub open: u64,
    pub resolved: u64,
    pub video: u64,
    pub audio: u64,
    pub subtitles: u64,
    pub other: u64,
}

// ---------------------------------------------------------------- users

/// Full user record for user management.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UserDetailDto {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub avatar: Option<String>,
    /// `local`, `jellyfin`, `emby`, `plex` or `oidc`.
    pub auth_source: String,
    pub external_id: Option<String>,
    #[cfg_attr(feature = "openapi", schema(value_type = u64))]
    pub permissions: Permissions,
    pub locale: Option<String>,
    /// Per-user quota overrides; `None` = global default.
    pub movie_quota_limit: Option<u32>,
    pub movie_quota_days: Option<u32>,
    pub tv_quota_limit: Option<u32>,
    pub tv_quota_days: Option<u32>,
    #[serde(default)]
    pub book_quota_limit: Option<u32>,
    #[serde(default)]
    pub book_quota_days: Option<u32>,
    #[serde(default)]
    pub music_quota_limit: Option<u32>,
    #[serde(default)]
    pub music_quota_days: Option<u32>,
    /// A local password is set.
    pub has_password: bool,
    pub request_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateUserDto {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    /// Defaults to the "default permissions" request setting.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<u64>))]
    pub permissions: Option<Permissions>,
    #[serde(default)]
    pub movie_quota_limit: Option<u32>,
    #[serde(default)]
    pub movie_quota_days: Option<u32>,
    #[serde(default)]
    pub tv_quota_limit: Option<u32>,
    #[serde(default)]
    pub tv_quota_days: Option<u32>,
    #[serde(default)]
    pub book_quota_limit: Option<u32>,
    #[serde(default)]
    pub book_quota_days: Option<u32>,
    #[serde(default)]
    pub music_quota_limit: Option<u32>,
    #[serde(default)]
    pub music_quota_days: Option<u32>,
}

/// `PUT /users/{id}`: replaces the editable fields (send the whole object back).
/// `password` is only changed when present (local users; also ends the user's other sessions).
/// Changing your own password requires `current_password` once the account has one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateUserDto {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    #[cfg_attr(feature = "openapi", schema(value_type = u64))]
    pub permissions: Permissions,
    #[serde(default)]
    pub movie_quota_limit: Option<u32>,
    #[serde(default)]
    pub movie_quota_days: Option<u32>,
    #[serde(default)]
    pub tv_quota_limit: Option<u32>,
    #[serde(default)]
    pub tv_quota_days: Option<u32>,
    #[serde(default)]
    pub book_quota_limit: Option<u32>,
    #[serde(default)]
    pub book_quota_days: Option<u32>,
    #[serde(default)]
    pub music_quota_limit: Option<u32>,
    #[serde(default)]
    pub music_quota_days: Option<u32>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub current_password: Option<String>,
}

/// A role preset (`GET /users/roles`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RolePresetDto {
    /// `admin`, `manager`, `power_user`, `user`.
    pub id: String,
    pub name: String,
    #[cfg_attr(feature = "openapi", schema(value_type = u64))]
    pub permissions: Permissions,
}

/// One permission bit with a label, for the permission checkboxes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PermissionInfoDto {
    pub bit: u64,
    /// Constant name, e.g. `REQUEST_4K`.
    pub key: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RolesDto {
    pub roles: Vec<RolePresetDto>,
    pub permissions: Vec<PermissionInfoDto>,
}

/// A media server users can be imported from (`GET /users/import/servers`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportServerDto {
    pub id: i64,
    pub name: String,
    /// `jellyfin`, `emby` or `plex`.
    pub kind: String,
}

/// Preview row for `GET /users/import/{media_server_id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExternalUserDto {
    pub external_id: String,
    pub username: String,
    pub email: Option<String>,
    pub is_admin: bool,
    pub avatar_url: Option<String>,
    /// Reelix user already linked to this account.
    pub existing_user_id: Option<i64>,
}

/// `POST /users/import/{media_server_id}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportUsersDto {
    /// External ids to import; `None` = every account not yet imported.
    #[serde(default)]
    pub external_ids: Option<Vec<String>>,
    /// Defaults to the "default permissions" request setting.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<u64>))]
    pub permissions: Option<Permissions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportUsersResultDto {
    pub imported: Vec<UserDetailDto>,
    /// Accounts skipped because they were already linked (or not selected).
    pub skipped: u32,
}

// ---------------------------------------------------------------- settings

/// Defaults used when an approved request is added to the library.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RequestDefaultsDto {
    pub profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
}

/// `GET/PUT /settings/requests` (admin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RequestSettingsDto {
    pub movie: RequestDefaultsDto,
    pub movie_4k: RequestDefaultsDto,
    pub series: RequestDefaultsDto,
    pub series_4k: RequestDefaultsDto,
    pub enable_4k_movie: bool,
    pub enable_4k_series: bool,
    /// Movie `minimum_availability` for added movies (`announced`, `in_cinemas`, `released`).
    pub minimum_availability: String,
    /// Series `series_type` for added series (`standard`, `daily`, `anime`).
    pub series_type: String,
    pub season_folder: bool,
    /// Auto-approve every request regardless of user permissions.
    pub auto_approve_movie: bool,
    pub auto_approve_series: bool,
    /// Start a search after an approved request has been added.
    pub search_on_approve: bool,
    /// Allow requesting individual seasons.
    pub partial_requests: bool,
    /// Permissions given to newly created / imported / external-login users.
    #[cfg_attr(feature = "openapi", schema(value_type = u64))]
    pub default_permissions: Permissions,
    /// Global quota defaults (`None` = unlimited).
    pub movie_quota_limit: Option<u32>,
    pub movie_quota_days: u32,
    pub tv_quota_limit: Option<u32>,
    pub tv_quota_days: u32,
    /// TMDB region / language for discovery (`None` = metadata defaults).
    pub discover_region: Option<String>,
    pub discover_language: Option<String>,
    /// Hide titles that are already available from discover rows.
    pub hide_available: bool,
    /// Book requests: defaults per format (ebook / audiobook quality profile + root folder).
    pub book_ebook: RequestDefaultsDto,
    pub book_audiobook: RequestDefaultsDto,
    /// Formats preselected in the book request dialog (empty = both).
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub book_default_formats: Vec<BookFormat>,
    /// Album requests: music quality profile + root folder (+ tags) for new artists.
    pub music: RequestDefaultsDto,
    /// Metadata profile for artists added by album requests (`None` = first profile).
    pub music_metadata_profile_id: Option<i64>,
    pub auto_approve_book: bool,
    pub auto_approve_music: bool,
    /// Global book / album quota defaults (`None` = unlimited).
    pub book_quota_limit: Option<u32>,
    pub book_quota_days: u32,
    pub music_quota_limit: Option<u32>,
    pub music_quota_days: u32,
    /// Radio requests: audiobook quality profile, radio root folder (`None` = the radio
    /// settings' library folder) and tags of shows followed by an approved request. The follow
    /// choice is the radio settings' `default_follow`.
    pub radio: RequestDefaultsDto,
    pub auto_approve_radio: bool,
    /// Global radio quota (`None` = unlimited).
    pub radio_quota_limit: Option<u32>,
    pub radio_quota_days: u32,
}

impl Default for RequestSettingsDto {
    fn default() -> Self {
        Self {
            movie: RequestDefaultsDto::default(),
            movie_4k: RequestDefaultsDto::default(),
            series: RequestDefaultsDto::default(),
            series_4k: RequestDefaultsDto::default(),
            enable_4k_movie: false,
            enable_4k_series: false,
            minimum_availability: "released".into(),
            series_type: "standard".into(),
            season_folder: true,
            auto_approve_movie: false,
            auto_approve_series: false,
            search_on_approve: true,
            partial_requests: true,
            default_permissions: Permissions::ROLE_USER,
            movie_quota_limit: None,
            movie_quota_days: 7,
            tv_quota_limit: None,
            tv_quota_days: 7,
            discover_region: None,
            discover_language: None,
            hide_available: false,
            book_ebook: RequestDefaultsDto::default(),
            book_audiobook: RequestDefaultsDto::default(),
            book_default_formats: Vec::new(),
            music: RequestDefaultsDto::default(),
            music_metadata_profile_id: None,
            auto_approve_book: false,
            auto_approve_music: false,
            book_quota_limit: None,
            book_quota_days: 7,
            music_quota_limit: None,
            music_quota_days: 7,
            radio: RequestDefaultsDto::default(),
            auto_approve_radio: false,
            radio_quota_limit: None,
            radio_quota_days: 7,
        }
    }
}

/// OIDC provider configuration. `client_secret` is masked (`********`) on read; sending the
/// mask back keeps the stored secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct OidcSettingsDto {
    pub enabled: bool,
    /// Issuer URL; discovery is fetched from `{issuer}/.well-known/openid-configuration`.
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    /// Extra scopes besides `openid` (default `email`, `profile`).
    pub scopes: Vec<String>,
    /// Login button text.
    pub button_label: String,
    /// Claim used as username: `preferred_username` (default), `email`, `sub`, `name`, `nickname`.
    pub username_claim: String,
    /// Create unknown users on first login (else only already-linked users may log in).
    pub auto_create: bool,
    /// Externally visible callback URL override, e.g. `https://reelix.example.com/api/v1/auth/oidc/callback`.
    /// Default: derived from the request's Host / X-Forwarded-* headers.
    pub redirect_url: Option<String>,
}

impl Default for OidcSettingsDto {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            scopes: vec!["email".into(), "profile".into()],
            button_label: "Single sign-on".into(),
            username_claim: "preferred_username".into(),
            auto_create: true,
            redirect_url: None,
        }
    }
}

/// `GET/PUT /settings/security` (admin): enabled login methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct SecuritySettingsDto {
    /// Local username/password login (admins can always log in locally).
    pub local_login: bool,
    pub jellyfin_login: bool,
    pub emby_login: bool,
    pub plex_login: bool,
    /// Media server to authenticate Jellyfin/Emby logins against (`None` = first of that kind).
    pub jellyfin_server_id: Option<i64>,
    pub emby_server_id: Option<i64>,
    /// Create unknown media-server users on first login.
    pub media_server_auto_create: bool,
    pub oidc: OidcSettingsDto,
}

impl Default for SecuritySettingsDto {
    fn default() -> Self {
        Self {
            local_login: true,
            jellyfin_login: false,
            emby_login: false,
            plex_login: false,
            jellyfin_server_id: None,
            emby_server_id: None,
            media_server_auto_create: true,
            oidc: OidcSettingsDto::default(),
        }
    }
}

// ---------------------------------------------------------------- external login

/// `GET /auth/methods` (public): which login buttons to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuthMethodsDto {
    pub local: bool,
    pub jellyfin: bool,
    pub emby: bool,
    pub plex: bool,
    pub oidc: bool,
    pub oidc_label: Option<String>,
}

/// `POST /auth/jellyfin` / `POST /auth/emby`. Responds with [`UserDto`] + session cookie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MediaServerLoginDto {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: bool,
}

/// `POST /auth/plex/pin` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlexPinRequestDto {
    /// Where app.plex.tv should send the browser after approval (popup close page).
    #[serde(default)]
    pub forward_url: Option<String>,
}

/// A Plex login PIN: open `auth_url` in a popup, then poll `POST /auth/plex/pin/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlexPinDto {
    pub id: i64,
    pub code: String,
    pub auth_url: String,
}

/// `POST /auth/plex/pin/{id}` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlexPinExchangeDto {
    #[serde(default)]
    pub remember_me: bool,
}

/// Result of polling a Plex PIN. `authenticated = false` → not approved yet, poll again.
/// When `authenticated`, the session cookie is set and `user` is filled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlexPinStatusDto {
    pub authenticated: bool,
    pub user: Option<UserDto>,
}

// ---------------------------------------------------------------- profile

/// Event ids usable in [`NotificationPrefsDto::events`].
pub const USER_NOTIFICATION_EVENTS: &[(&str, &str)] = &[
    ("request_approved", "My request was approved"),
    ("request_declined", "My request was declined"),
    ("request_available", "My request is available"),
    ("request_failed", "My request failed"),
    ("issue_comment", "New comment on my issue"),
    ("issue_resolved", "My issue was resolved"),
    ("request_pending", "A request needs approval (managers)"),
    ("issue_reported", "An issue was reported (managers)"),
];

/// `GET/PUT /profile/notifications`: the current user's notification and watchlist preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NotificationPrefsDto {
    /// Enabled event ids (see `USER_NOTIFICATION_EVENTS`).
    pub events: Vec<String>,
    /// Personal delivery targets, used by notifiers that support per-user targets.
    pub email: Option<String>,
    pub discord_user_id: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub pushover_user_key: Option<String>,
    /// Turn Plex watchlist items into requests (Plex-linked accounts only).
    pub watchlist_sync_movies: bool,
    pub watchlist_sync_series: bool,
}

impl Default for NotificationPrefsDto {
    fn default() -> Self {
        Self {
            events: vec![
                "request_approved".into(),
                "request_declined".into(),
                "request_available".into(),
                "request_failed".into(),
                "issue_comment".into(),
                "issue_resolved".into(),
            ],
            email: None,
            discord_user_id: None,
            telegram_chat_id: None,
            pushover_user_key: None,
            watchlist_sync_movies: false,
            watchlist_sync_series: false,
        }
    }
}
