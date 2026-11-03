//! TheTVDB v4 client (optional): bearer-token login, extended series and
//! episodes in default/absolute/DVD/… orderings.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reelix_core::MediaKind;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::http::{RetryPolicy, TtlCache, send};
use crate::model::*;
use crate::{MetaError, MetadataProvider};

pub const DEFAULT_BASE_URL: &str = "https://api4.thetvdb.com/v4";

/// Episode ordering (TVDB "season type").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TvdbOrdering {
    /// Aired order (the series' default season type).
    #[default]
    Default,
    Official,
    Dvd,
    /// Everything in season 1, episode number = absolute number (anime).
    Absolute,
    Alternate,
    Regional,
}

impl TvdbOrdering {
    fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Official => "official",
            Self::Dvd => "dvd",
            Self::Absolute => "absolute",
            Self::Alternate => "alternate",
            Self::Regional => "regional",
        }
    }

    /// Season `type.type` value this ordering corresponds to.
    fn season_type(self) -> &'static str {
        match self {
            Self::Default => "official",
            other => other.as_str(),
        }
    }
}

/// Tunables for [`Tvdb`].
#[derive(Debug, Clone)]
pub struct TvdbOptions {
    pub base_url: String,
    /// e.g. `en-US`; mapped to TVDB's three-letter codes.
    pub language: String,
    pub retry: RetryPolicy,
    /// Tokens are valid for a month; re-login before that.
    pub token_lifetime: Duration,
    pub cache_ttl: Duration,
    pub cache_entries: usize,
}

impl Default for TvdbOptions {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            language: "en-US".into(),
            retry: RetryPolicy::default(),
            token_lifetime: Duration::from_secs(25 * 24 * 3600),
            cache_ttl: Duration::from_secs(6 * 3600),
            cache_entries: 300,
        }
    }
}

/// ISO 639-1 (optionally with region) → TVDB ISO 639-2 code.
pub fn tvdb_language(lang: &str) -> String {
    let two = lang.split(['-', '_']).next().unwrap_or("en").to_ascii_lowercase();
    let three = match two.as_str() {
        "en" => "eng",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "ja" => "jpn",
        "pt" => "por",
        "nl" => "nld",
        "sv" => "swe",
        "da" => "dan",
        "no" | "nb" | "nn" => "nor",
        "fi" => "fin",
        "pl" => "pol",
        "ru" => "rus",
        "ko" => "kor",
        "zh" => "zho",
        "cs" => "ces",
        "hu" => "hun",
        "tr" => "tur",
        "he" => "heb",
        "ar" => "ara",
        "el" => "ell",
        "uk" => "ukr",
        "th" => "tha",
        other if other.len() == 3 => other,
        _ => "eng",
    };
    three.to_string()
}

/// TheTVDB v4 client. Cheap to clone; clones share the token and cache.
#[derive(Debug, Clone)]
pub struct Tvdb {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    api_key: String,
    pin: Option<String>,
    opts: TvdbOptions,
    token: Mutex<Option<(String, Instant)>>,
    cache: TtlCache,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tvdb")
            .field("base_url", &self.opts.base_url)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
    #[serde(default)]
    links: Option<Links>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Links {
    next: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WError {
    message: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct WRemoteId {
    id: String,
    source_name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WNamed {
    id: u32,
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WAlias {
    language: Option<String>,
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WArtwork {
    image: String,
    #[serde(rename = "type")]
    kind: u32,
    language: Option<String>,
    score: f64,
    width: u32,
    height: u32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WSeasonType {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WSeason {
    id: u32,
    number: u32,
    #[serde(rename = "type")]
    kind: WSeasonType,
    image: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct WTranslation {
    language: String,
    name: Option<String>,
    overview: Option<String>,
    is_alias: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct WTranslations {
    name_translations: Vec<WTranslation>,
    overview_translations: Vec<WTranslation>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct WSeries {
    id: u32,
    name: String,
    slug: Option<String>,
    image: Option<String>,
    overview: Option<String>,
    first_aired: Option<String>,
    last_aired: Option<String>,
    next_aired: Option<String>,
    status: Option<WNamed>,
    original_country: Option<String>,
    original_language: Option<String>,
    average_runtime: Option<u32>,
    year: Option<String>,
    score: f64,
    aliases: Vec<WAlias>,
    genres: Vec<WNamed>,
    latest_network: Option<WNamed>,
    original_network: Option<WNamed>,
    remote_ids: Vec<WRemoteId>,
    artworks: Vec<WArtwork>,
    seasons: Vec<WSeason>,
    translations: Option<WTranslations>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct WEpisode {
    id: u32,
    name: Option<String>,
    overview: Option<String>,
    aired: Option<String>,
    runtime: Option<u32>,
    image: Option<String>,
    number: u32,
    season_number: u32,
    absolute_number: Option<u32>,
    finale_type: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WEpisodesPage {
    episodes: Vec<WEpisode>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WSearchHit {
    tvdb_id: Option<String>,
    name: String,
    year: Option<String>,
    first_air_time: Option<String>,
    overview: Option<String>,
    image_url: Option<String>,
    primary_language: Option<String>,
    remote_ids: Vec<WRemoteId>,
    translations: BTreeMap<String, String>,
    overviews: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WRemoteHit {
    series: Option<WNamed>,
}

fn remote(ids: &[WRemoteId], source: &str) -> Option<String> {
    ids.iter()
        .find(|r| r.source_name.eq_ignore_ascii_case(source))
        .map(|r| r.id.clone())
}

fn status(s: Option<&WNamed>) -> SeriesStatus {
    match s.map(|s| s.name.as_str()) {
        Some("Continuing") => SeriesStatus::Continuing,
        Some("Ended") => SeriesStatus::Ended,
        Some("Upcoming") => SeriesStatus::Upcoming,
        _ => SeriesStatus::Unknown,
    }
}

/// TVDB artwork type ids for series.
const ART_POSTER: u32 = 2;
const ART_BACKGROUND: u32 = 3;
const ART_CLEARLOGO: u32 = 23;

impl Tvdb {
    pub fn new(http: reqwest::Client, api_key: String, pin: Option<String>, opts: TvdbOptions) -> Self {
        let cache = TtlCache::new(opts.cache_entries);
        Self {
            inner: Arc::new(Inner {
                http,
                api_key,
                pin,
                opts,
                token: Mutex::new(None),
                cache,
            }),
        }
    }

    fn lang(&self) -> String {
        tvdb_language(&self.inner.opts.language)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.inner.opts.base_url.trim_end_matches('/'), path)
    }

    /// Logs in (`POST /login`) and returns a bearer token, reusing a fresh one.
    pub async fn token(&self) -> Result<String, MetaError> {
        let mut guard = self.inner.token.lock().await;
        if let Some((t, at)) = guard.as_ref()
            && at.elapsed() < self.inner.opts.token_lifetime
        {
            return Ok(t.clone());
        }
        let mut body = json!({ "apikey": self.inner.api_key });
        if let Some(pin) = self.inner.pin.as_deref().filter(|p| !p.is_empty()) {
            body["pin"] = json!(pin);
        }
        let url = self.url("/login");
        let resp = send(
            || self.inner.http.post(&url).json(&body),
            None,
            &self.inner.opts.retry,
        )
        .await?;
        let st = resp.status();
        let bytes = resp.bytes().await?;
        if !st.is_success() {
            let msg = serde_json::from_slice::<WError>(&bytes)
                .map(|e| e.message)
                .unwrap_or_default();
            return Err(if st == StatusCode::UNAUTHORIZED {
                MetaError::Unauthorized(msg)
            } else {
                MetaError::Status {
                    status: st.as_u16(),
                    message: msg,
                }
            });
        }
        #[derive(Deserialize)]
        struct Tok {
            token: String,
        }
        let tok: Envelope<Tok> = serde_json::from_slice(&bytes)?;
        *guard = Some((tok.data.token.clone(), Instant::now()));
        Ok(tok.data.token)
    }

    /// Authenticated GET; re-logs in once when the token was rejected.
    async fn get_value(&self, path_and_query: &str) -> Result<Arc<Value>, MetaError> {
        if let Some(v) = self.inner.cache.get(path_and_query) {
            return Ok(v);
        }
        let url = self.url(path_and_query);
        for attempt in 0..2 {
            let token = self.token().await?;
            let resp = send(
                || {
                    self.inner
                        .http
                        .get(&url)
                        .bearer_auth(&token)
                        .header("Accept", "application/json")
                },
                None,
                &self.inner.opts.retry,
            )
            .await?;
            let st = resp.status();
            if st == StatusCode::UNAUTHORIZED && attempt == 0 {
                tracing::debug!("tvdb token rejected, logging in again");
                *self.inner.token.lock().await = None;
                continue;
            }
            let bytes = resp.bytes().await?;
            if !st.is_success() {
                let message = serde_json::from_slice::<WError>(&bytes)
                    .map(|e| e.message)
                    .unwrap_or_default();
                return Err(match st {
                    StatusCode::NOT_FOUND => MetaError::NotFound,
                    StatusCode::UNAUTHORIZED => MetaError::Unauthorized(message),
                    _ => MetaError::Status {
                        status: st.as_u16(),
                        message,
                    },
                });
            }
            let v = Arc::new(serde_json::from_slice::<Value>(&bytes)?);
            self.inner
                .cache
                .insert(path_and_query.to_string(), v.clone(), self.inner.opts.cache_ttl);
            return Ok(v);
        }
        Err(MetaError::Unauthorized("token rejected".into()))
    }

    async fn get<T: DeserializeOwned>(&self, path_and_query: &str) -> Result<Envelope<T>, MetaError> {
        let v = self.get_value(path_and_query).await?;
        Ok(Envelope::<T>::deserialize(&*v)?)
    }

    async fn extended(&self, id: TvdbId) -> Result<WSeries, MetaError> {
        Ok(self
            .get::<WSeries>(&format!("/series/{id}/extended?meta=translations&short=true"))
            .await?
            .data)
    }

    async fn episodes_raw(&self, id: TvdbId, ordering: TvdbOrdering) -> Result<Vec<WEpisode>, MetaError> {
        let mut out = Vec::new();
        let (lang, kind) = (self.lang(), ordering.as_str());
        // Follow `links.next` page by page; bounded to avoid runaway loops.
        for page in 0..100 {
            let env = self
                .get::<WEpisodesPage>(&format!("/series/{id}/episodes/{kind}/{lang}?page={page}"))
                .await?;
            let has_next = env.links.and_then(|l| l.next).is_some_and(|n| !n.is_empty());
            let empty = env.data.episodes.is_empty();
            out.extend(env.data.episodes);
            if !has_next || empty {
                break;
            }
        }
        Ok(out)
    }

    /// Episodes of a series in the given ordering, grouped by season.
    pub async fn episodes(
        &self,
        id: TvdbId,
        ordering: TvdbOrdering,
    ) -> Result<Vec<EpisodeMetadata>, MetaError> {
        Ok(self
            .episodes_raw(id, ordering)
            .await?
            .into_iter()
            .map(|e| {
                let absolute = e
                    .absolute_number
                    .filter(|n| *n > 0)
                    .or((ordering == TvdbOrdering::Absolute && e.season_number > 0).then_some(e.number));
                EpisodeMetadata {
                    season_number: e.season_number,
                    episode_number: e.number,
                    absolute_number: absolute,
                    title: non_empty(e.name),
                    overview: non_empty(e.overview),
                    air_date: parse_date(e.aired.as_deref()),
                    runtime_minutes: e.runtime.filter(|r| *r > 0),
                    still: non_empty(e.image),
                    tmdb_id: None,
                    tvdb_id: Some(e.id),
                    finale: non_empty(e.finale_type),
                    vote_average: None,
                }
            })
            .collect())
    }

    /// Extended series plus all episodes in `ordering`.
    pub async fn series_with_ordering(
        &self,
        id: TvdbId,
        ordering: TvdbOrdering,
    ) -> Result<SeriesMetadata, MetaError> {
        let raw = self.extended(id).await?;
        let episodes = self.episodes(id, ordering).await?;
        Ok(self.convert(raw, episodes, ordering))
    }

    fn convert(
        &self,
        raw: WSeries,
        episodes: Vec<EpisodeMetadata>,
        ordering: TvdbOrdering,
    ) -> SeriesMetadata {
        let lang = self.lang();
        let tr = raw.translations.unwrap_or_default();
        let title = tr
            .name_translations
            .iter()
            .find(|t| t.language == lang && !t.is_alias.unwrap_or(false))
            .and_then(|t| t.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| raw.name.clone());
        let overview = tr
            .overview_translations
            .iter()
            .find(|t| t.language == lang)
            .and_then(|t| t.overview.clone())
            .or(raw.overview);

        let mut by_season: BTreeMap<u32, Vec<EpisodeMetadata>> = BTreeMap::new();
        for e in episodes {
            by_season.entry(e.season_number).or_default().push(e);
        }
        let wanted = ordering.season_type();
        let seasons = by_season
            .into_iter()
            .map(|(n, mut eps)| {
                eps.sort_by_key(|e| e.episode_number);
                let info = raw
                    .seasons
                    .iter()
                    .find(|s| s.number == n && s.kind.kind == wanted);
                SeasonMetadata {
                    season_number: n,
                    name: info.and_then(|s| non_empty(s.name.clone())),
                    overview: None,
                    air_date: eps.iter().filter_map(|e| e.air_date).min(),
                    poster: info.and_then(|s| non_empty(s.image.clone())),
                    episode_count: eps.len() as u32,
                    episodes: eps,
                    tmdb_id: None,
                    tvdb_id: info.map(|s| s.id),
                }
            })
            .collect();

        let art = |kind: u32| -> Vec<Image> {
            let mut v: Vec<_> = raw
                .artworks
                .iter()
                .filter(|a| a.kind == kind && !a.image.is_empty())
                .map(|a| Image {
                    url: a.image.clone(),
                    path: a.image.clone(),
                    width: a.width,
                    height: a.height,
                    language: non_empty(a.language.clone()),
                    vote_average: a.score,
                })
                .collect();
            v.sort_by(|a, b| b.vote_average.total_cmp(&a.vote_average));
            v
        };
        let (posters, backdrops, logos) = (art(ART_POSTER), art(ART_BACKGROUND), art(ART_CLEARLOGO));
        let imdb = remote(&raw.remote_ids, "IMDB");
        let tmdb = remote(&raw.remote_ids, "TheMovieDB.com").and_then(|s| s.parse().ok());
        let first = parse_date(raw.first_aired.as_deref());
        let network = raw.latest_network.as_ref().or(raw.original_network.as_ref());
        SeriesMetadata {
            tmdb_id: tmdb,
            tvdb_id: Some(raw.id),
            imdb_id: imdb.clone(),
            external_ids: ExternalIds {
                tmdb,
                imdb,
                tvdb: Some(raw.id),
                wikidata: None,
            },
            sort_title: sort_title(&title),
            original_title: (raw.name != title).then(|| raw.name.clone()),
            title,
            overview: non_empty(overview),
            year: year_of(first).or_else(|| raw.year.as_deref().and_then(|y| y.parse().ok())),
            first_air_date: first,
            last_air_date: parse_date(raw.last_aired.as_deref()),
            next_air_date: parse_date(raw.next_aired.as_deref()),
            status: status(raw.status.as_ref()),
            show_type: None,
            network: network.map(|n| n.name.clone()),
            networks: network
                .map(|n| Company {
                    id: n.id,
                    name: n.name.clone(),
                    logo_url: None,
                    origin_country: None,
                })
                .into_iter()
                .collect(),
            genres: raw
                .genres
                .into_iter()
                .map(|g| Genre {
                    id: g.id,
                    name: g.name,
                })
                .collect(),
            keywords: Vec::new(),
            runtime_minutes: raw.average_runtime.filter(|r| *r > 0),
            certification: None,
            original_language: non_empty(raw.original_language),
            origin_country: non_empty(raw.original_country).into_iter().collect(),
            alternative_titles: raw
                .aliases
                .into_iter()
                .map(|a| AlternativeTitle {
                    title: a.name,
                    country: None,
                    kind: non_empty(a.language),
                })
                .collect(),
            credits: Credits::default(),
            images: Images {
                poster: non_empty(raw.image).or_else(|| posters.first().map(|p| p.url.clone())),
                backdrop: backdrops.first().map(|b| b.url.clone()),
                logo: logos.first().map(|l| l.url.clone()),
                posters,
                backdrops,
                logos,
            },
            videos: Vec::new(),
            ratings: Ratings {
                vote_average: 0.0,
                vote_count: 0,
                popularity: raw.score,
            },
            seasons,
            recommendations: Vec::new(),
            similar: Vec::new(),
        }
    }

    /// Series search (`/search?type=series`).
    pub async fn search_series(&self, query: &str) -> Result<Vec<SearchHit>, MetaError> {
        let q = reqwest::Url::parse_with_params("http://x/search", &[("query", query), ("type", "series")])
            .map_err(|e| MetaError::Decode(e.to_string()))?;
        let env = self
            .get::<Vec<WSearchHit>>(&format!("/search?{}", q.query().unwrap_or_default()))
            .await?;
        let lang = self.lang();
        Ok(env
            .data
            .into_iter()
            .map(|h| {
                let release_date = parse_date(h.first_air_time.as_deref());
                SearchHit {
                    kind: MediaKind::Series,
                    tmdb_id: remote(&h.remote_ids, "TheMovieDB.com").and_then(|s| s.parse().ok()),
                    tvdb_id: h.tvdb_id.as_deref().and_then(|s| s.parse().ok()),
                    imdb_id: remote(&h.remote_ids, "IMDB"),
                    title: h.translations.get(&lang).cloned().unwrap_or(h.name.clone()),
                    original_title: Some(h.name),
                    year: year_of(release_date).or_else(|| h.year.as_deref().and_then(|y| y.parse().ok())),
                    release_date,
                    overview: h.overviews.get(&lang).cloned().or(non_empty(h.overview)),
                    poster: non_empty(h.image_url),
                    backdrop: None,
                    genre_ids: Vec::new(),
                    original_language: non_empty(h.primary_language),
                    vote_average: 0.0,
                    vote_count: 0,
                    popularity: 0.0,
                    adult: false,
                }
            })
            .collect())
    }

    /// Resolves an IMDb id to a TVDB series id via `/search/remoteid`.
    pub async fn find_by_imdb(&self, imdb: &str) -> Result<TvdbId, MetaError> {
        let env = self
            .get::<Vec<WRemoteHit>>(&format!("/search/remoteid/{imdb}"))
            .await?;
        env.data
            .into_iter()
            .find_map(|h| h.series.map(|s| s.id))
            .ok_or(MetaError::NotFound)
    }
}

#[async_trait]
impl MetadataProvider for Tvdb {
    fn name(&self) -> &'static str {
        "tvdb"
    }

    async fn movie(&self, _id: TmdbId) -> Result<MovieMetadata, MetaError> {
        Err(MetaError::Unsupported("movie lookup"))
    }

    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        let tvdb = match id {
            SeriesExternalId::Tvdb(id) => id,
            SeriesExternalId::Imdb(imdb) => self.find_by_imdb(&imdb).await?,
            SeriesExternalId::Tmdb(_) => return Err(MetaError::Unsupported("lookup by TMDB id")),
        };
        self.series_with_ordering(tvdb, TvdbOrdering::Default).await
    }

    async fn search(&self, q: &str, kind: MediaKind) -> Result<Vec<SearchHit>, MetaError> {
        match kind {
            MediaKind::Series => self.search_series(q).await,
            MediaKind::Movie => Err(MetaError::Unsupported("movie search")),
        }
    }

    async fn discover(&self, _feed: DiscoverFeed, _page: u32) -> Result<Page<SearchHit>, MetaError> {
        Err(MetaError::Unsupported("discover feeds"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes() {
        assert_eq!(tvdb_language("en-US"), "eng");
        assert_eq!(tvdb_language("de"), "deu");
        assert_eq!(tvdb_language("pt_BR"), "por");
        assert_eq!(tvdb_language("jpn"), "jpn");
        assert_eq!(tvdb_language("xx"), "eng");
    }
}
