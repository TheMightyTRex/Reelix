//! Provider-neutral metadata types returned by every [`crate::MetadataProvider`].
//!
//! All image fields hold absolute URLs; dates are calendar dates (no time zone).

use chrono::NaiveDate;
use reelix_core::MediaKind;
use serde::{Deserialize, Serialize};

/// A TMDB numeric id.
pub type TmdbId = u32;
/// A TVDB numeric id.
pub type TvdbId = u32;

/// The id a series is looked up by; TMDB resolves TVDB/IMDb ids through `/find`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "source", content = "id", rename_all = "lowercase")]
pub enum SeriesExternalId {
    Tmdb(TmdbId),
    Tvdb(TvdbId),
    Imdb(String),
}

/// One page of a paged TMDB listing (pages are 1-based).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    pub page: u32,
    pub total_pages: u32,
    pub total_results: u32,
    pub results: Vec<T>,
}

impl<T> Page<T> {
    /// Converts the items while keeping the paging info.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            page: self.page,
            total_pages: self.total_pages,
            total_results: self.total_results,
            results: self.results.into_iter().map(f).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Genre {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyword {
    pub id: u32,
    pub name: String,
}

/// A production company (studio) or TV network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Company {
    pub id: u32,
    pub name: String,
    pub logo_url: Option<String>,
    pub origin_country: Option<String>,
}

/// Ids of the same title on other services.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalIds {
    pub tmdb: Option<TmdbId>,
    pub imdb: Option<String>,
    pub tvdb: Option<TvdbId>,
    pub wikidata: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlternativeTitle {
    pub title: String,
    /// ISO 3166-1 country code, when known.
    pub country: Option<String>,
    /// Free-form qualifier such as "working title" or "romaji".
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ratings {
    pub vote_average: f64,
    pub vote_count: u32,
    pub popularity: f64,
}

/// A single artwork with its absolute (original size) URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub url: String,
    /// Provider-relative path; combine with another size via [`crate::image_url`].
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub language: Option<String>,
    pub vote_average: f64,
}

/// Primary artwork (sized URLs) plus every alternative the provider returned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Images {
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub logo: Option<String>,
    pub posters: Vec<Image>,
    pub backdrops: Vec<Image>,
    pub logos: Vec<Image>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CastMember {
    pub tmdb_id: TmdbId,
    pub name: String,
    pub character: Option<String>,
    pub order: u32,
    pub profile_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrewMember {
    pub tmdb_id: TmdbId,
    pub name: String,
    pub job: String,
    pub department: String,
    pub profile_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    pub cast: Vec<CastMember>,
    pub crew: Vec<CrewMember>,
}

/// A trailer, teaser, clip, … hosted on YouTube or Vimeo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Video {
    pub key: String,
    pub site: String,
    /// "Trailer", "Teaser", "Clip", "Featurette", …
    pub kind: String,
    pub name: String,
    pub language: Option<String>,
    pub official: bool,
    /// Watch URL for known sites.
    pub url: Option<String>,
}

/// TMDB release type numbers 1–6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseType {
    Premiere,
    TheatricalLimited,
    Theatrical,
    Digital,
    Physical,
    Tv,
}

impl ReleaseType {
    pub fn from_tmdb(n: u8) -> Option<Self> {
        Some(match n {
            1 => Self::Premiere,
            2 => Self::TheatricalLimited,
            3 => Self::Theatrical,
            4 => Self::Digital,
            5 => Self::Physical,
            6 => Self::Tv,
            _ => return None,
        })
    }
}

/// One dated release of a movie in one country.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionalRelease {
    pub country: String,
    pub kind: ReleaseType,
    pub date: NaiveDate,
    pub certification: Option<String>,
    pub note: Option<String>,
}

/// Release dates resolved for the configured region (falling back to the earliest worldwide).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReleaseDates {
    pub cinema: Option<NaiveDate>,
    pub digital: Option<NaiveDate>,
    pub physical: Option<NaiveDate>,
    pub all: Vec<RegionalRelease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovieStatus {
    Rumored,
    Planned,
    InProduction,
    PostProduction,
    Released,
    Canceled,
    Unknown,
}

/// Lightweight reference to the collection a movie belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionRef {
    pub tmdb_id: TmdbId,
    pub name: String,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MovieMetadata {
    pub tmdb_id: TmdbId,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: String,
    pub original_language: Option<String>,
    pub sort_title: String,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub year: Option<i32>,
    /// Primary (TMDB "release_date") date.
    pub release_date: Option<NaiveDate>,
    pub release_dates: ReleaseDates,
    /// Certification in the configured region, e.g. "PG-13".
    pub certification: Option<String>,
    pub status: MovieStatus,
    pub runtime_minutes: Option<u32>,
    pub genres: Vec<Genre>,
    pub keywords: Vec<Keyword>,
    pub studios: Vec<Company>,
    pub collection: Option<CollectionRef>,
    pub alternative_titles: Vec<AlternativeTitle>,
    pub external_ids: ExternalIds,
    pub credits: Credits,
    pub images: Images,
    pub videos: Vec<Video>,
    pub ratings: Ratings,
    pub adult: bool,
    pub homepage: Option<String>,
    pub recommendations: Vec<SearchHit>,
    pub similar: Vec<SearchHit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionMetadata {
    pub tmdb_id: TmdbId,
    pub name: String,
    pub overview: Option<String>,
    pub images: Images,
    /// Movies of the collection ordered by release date.
    pub parts: Vec<SearchHit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesStatus {
    Continuing,
    Ended,
    Upcoming,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeMetadata {
    pub season_number: u32,
    pub episode_number: u32,
    /// From TVDB when available; for TMDB derived from the order of regular seasons.
    pub absolute_number: Option<u32>,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<NaiveDate>,
    pub runtime_minutes: Option<u32>,
    pub still: Option<String>,
    pub tmdb_id: Option<TmdbId>,
    pub tvdb_id: Option<TvdbId>,
    /// "finale", "mid_season", "series", … when the provider flags it.
    pub finale: Option<String>,
    pub vote_average: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeasonMetadata {
    pub season_number: u32,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<NaiveDate>,
    pub poster: Option<String>,
    pub episode_count: u32,
    pub episodes: Vec<EpisodeMetadata>,
    pub tmdb_id: Option<TmdbId>,
    pub tvdb_id: Option<TvdbId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesMetadata {
    pub tmdb_id: Option<TmdbId>,
    pub tvdb_id: Option<TvdbId>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub sort_title: String,
    pub overview: Option<String>,
    pub year: Option<i32>,
    pub first_air_date: Option<NaiveDate>,
    pub last_air_date: Option<NaiveDate>,
    pub next_air_date: Option<NaiveDate>,
    pub status: SeriesStatus,
    /// Provider's own show type, e.g. "Scripted", "Miniseries", "Reality".
    pub show_type: Option<String>,
    /// Primary (latest) network name.
    pub network: Option<String>,
    pub networks: Vec<Company>,
    pub genres: Vec<Genre>,
    pub keywords: Vec<Keyword>,
    pub runtime_minutes: Option<u32>,
    pub certification: Option<String>,
    pub original_language: Option<String>,
    pub origin_country: Vec<String>,
    pub alternative_titles: Vec<AlternativeTitle>,
    pub external_ids: ExternalIds,
    pub credits: Credits,
    pub images: Images,
    pub videos: Vec<Video>,
    pub ratings: Ratings,
    pub seasons: Vec<SeasonMetadata>,
    pub recommendations: Vec<SearchHit>,
    pub similar: Vec<SearchHit>,
}

impl SeriesMetadata {
    /// All episodes across seasons in season/episode order.
    pub fn episodes(&self) -> impl Iterator<Item = &EpisodeMetadata> {
        self.seasons.iter().flat_map(|s| s.episodes.iter())
    }
}

/// A movie or series in search results and discover feeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub kind: MediaKind,
    pub tmdb_id: Option<TmdbId>,
    pub tvdb_id: Option<TvdbId>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub release_date: Option<NaiveDate>,
    pub overview: Option<String>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub genre_ids: Vec<u32>,
    pub original_language: Option<String>,
    pub vote_average: f64,
    pub vote_count: u32,
    pub popularity: f64,
    pub adult: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonSummary {
    pub tmdb_id: TmdbId,
    pub name: String,
    pub known_for_department: Option<String>,
    pub profile_url: Option<String>,
    pub popularity: f64,
}

/// A `search/multi` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "media_type", rename_all = "snake_case")]
pub enum MultiHit {
    Media(SearchHit),
    Person(PersonSummary),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub tmdb_id: TmdbId,
    pub imdb_id: Option<String>,
    pub name: String,
    pub also_known_as: Vec<String>,
    pub biography: Option<String>,
    pub birthday: Option<NaiveDate>,
    pub deathday: Option<NaiveDate>,
    pub place_of_birth: Option<String>,
    pub known_for_department: Option<String>,
    pub profile_url: Option<String>,
    pub popularity: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeWindow {
    Day,
    Week,
}

/// A discovery feed (Jellyseerr-style home page rows).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "feed", rename_all = "snake_case")]
pub enum DiscoverFeed {
    /// `kind: None` mixes movies and series.
    Trending {
        kind: Option<MediaKind>,
        window: TimeWindow,
    },
    Popular {
        kind: MediaKind,
    },
    TopRated {
        kind: MediaKind,
    },
    /// Movies only.
    Upcoming,
    /// Movies only.
    NowPlaying,
    /// Series only.
    OnTheAir,
    /// Series only.
    AiringToday,
    Genre {
        kind: MediaKind,
        genre_id: u32,
    },
    /// Series by TV network.
    Network {
        network_id: u32,
    },
    /// Movies by production company.
    Studio {
        company_id: u32,
    },
    Keyword {
        kind: MediaKind,
        keyword_id: u32,
    },
}

/// Title used for sorting: lower case without a leading English article.
pub fn sort_title(title: &str) -> String {
    let lower = title.trim().to_lowercase();
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = lower.strip_prefix(article)
            && !rest.is_empty()
        {
            return rest.to_string();
        }
    }
    lower
}

/// Lenient `YYYY-MM-DD` (or ISO timestamp) parsing; empty strings become `None`.
pub(crate) fn parse_date(s: Option<&str>) -> Option<NaiveDate> {
    let s = s?.trim();
    let day = s.get(..10)?;
    NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()
}

pub(crate) fn year_of(d: Option<NaiveDate>) -> Option<i32> {
    use chrono::Datelike;
    d.map(|d| d.year())
}

/// `Some(s)` unless blank.
pub(crate) fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_title_strips_articles() {
        assert_eq!(sort_title("The Matrix"), "matrix");
        assert_eq!(sort_title("A Quiet Place"), "quiet place");
        assert_eq!(sort_title("Anora"), "anora");
        assert_eq!(sort_title("The"), "the");
    }

    #[test]
    fn dates_are_lenient() {
        assert_eq!(parse_date(Some("")), None);
        assert_eq!(
            parse_date(Some("1999-03-30")),
            NaiveDate::from_ymd_opt(1999, 3, 30)
        );
        assert_eq!(
            parse_date(Some("1999-03-31T00:00:00.000Z")),
            NaiveDate::from_ymd_opt(1999, 3, 31)
        );
        assert_eq!(parse_date(Some("garbage")), None);
    }

    #[test]
    fn discover_feed_serializes_tagged() {
        let f = DiscoverFeed::Genre {
            kind: MediaKind::Movie,
            genre_id: 28,
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"feed": "genre", "kind": "movie", "genre_id": 28})
        );
        assert_eq!(serde_json::from_value::<DiscoverFeed>(v).unwrap(), f);
        let up: DiscoverFeed = serde_json::from_str(r#"{"feed":"upcoming"}"#).unwrap();
        assert_eq!(up, DiscoverFeed::Upcoming);
    }
}
