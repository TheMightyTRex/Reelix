//! TMDB v3 client: details, search, discovery, rate limiting and caching.

mod wire;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reelix_core::MediaKind;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::http::{RateLimiter, RetryPolicy, TtlCache, send};
use crate::model::*;
use crate::{MetaError, MetadataProvider};
use wire::Img;

pub const DEFAULT_BASE_URL: &str = "https://api.themoviedb.org/3";
pub const DEFAULT_IMAGE_BASE_URL: &str = "https://image.tmdb.org/t/p/";

/// Joins an image base, a size (`w500`, `original`, …) and a TMDB file path.
pub fn image_url(base: &str, size: &str, path: &str) -> String {
    format!(
        "{}/{}/{}",
        base.trim_end_matches('/'),
        size,
        path.trim_start_matches('/')
    )
}

/// How requests authenticate.
#[derive(Clone)]
pub enum TmdbAuth {
    /// v3 API key sent as the `api_key` query parameter.
    ApiKey(String),
    /// v4 read access token sent as `Authorization: Bearer`.
    Bearer(String),
}

impl std::fmt::Debug for TmdbAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ApiKey(_) => "ApiKey(***)",
            Self::Bearer(_) => "Bearer(***)",
        })
    }
}

/// Tunables for [`Tmdb`].
#[derive(Debug, Clone)]
pub struct TmdbOptions {
    pub base_url: String,
    pub image_base_url: String,
    /// e.g. `en-US`.
    pub language: String,
    /// ISO 3166-1 region; `US` is assumed for release dates when unset.
    pub region: Option<String>,
    pub include_adult: bool,
    /// Requests allowed per `rate_period`.
    pub rate_limit: usize,
    pub rate_period: Duration,
    pub retry: RetryPolicy,
    /// TTL for details (movies, series, collections, persons, genres).
    pub details_ttl: Duration,
    /// TTL for searches and discover feeds.
    pub feed_ttl: Duration,
    /// 0 disables caching.
    pub cache_entries: usize,
}

impl Default for TmdbOptions {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            image_base_url: DEFAULT_IMAGE_BASE_URL.into(),
            language: "en-US".into(),
            region: None,
            include_adult: false,
            rate_limit: 40,
            rate_period: Duration::from_secs(10),
            retry: RetryPolicy::default(),
            details_ttl: Duration::from_secs(6 * 3600),
            feed_ttl: Duration::from_secs(30 * 60),
            cache_entries: 2000,
        }
    }
}

/// External id sources accepted by `/find`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSource {
    Imdb,
    Tvdb,
}

/// Results of a `/find` lookup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FindResults {
    pub movies: Vec<SearchHit>,
    pub series: Vec<SearchHit>,
}

/// Extra data appended to movie details.
const MOVIE_APPEND: &str =
    "release_dates,alternative_titles,external_ids,credits,images,videos,keywords,recommendations,similar";
/// Extra data appended to series details.
const TV_APPEND: &str =
    "external_ids,alternative_titles,content_ratings,credits,images,videos,keywords,recommendations,similar";
/// TMDB allows at most 20 `append_to_response` items per request.
const MAX_APPEND: usize = 20;

/// TMDB v3 client. Cheap to clone; clones share the rate limiter and cache.
#[derive(Debug, Clone)]
pub struct Tmdb {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    auth: TmdbAuth,
    opts: TmdbOptions,
    limiter: RateLimiter,
    cache: TtlCache,
}

fn kind_path(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movie",
        MediaKind::Series => "tv",
    }
}

impl Tmdb {
    pub fn new(http: reqwest::Client, auth: TmdbAuth, opts: TmdbOptions) -> Self {
        let limiter = RateLimiter::new(opts.rate_limit, opts.rate_period);
        let cache = TtlCache::new(opts.cache_entries);
        Self {
            inner: Arc::new(Inner {
                http,
                auth,
                opts,
                limiter,
                cache,
            }),
        }
    }

    pub fn options(&self) -> &TmdbOptions {
        &self.inner.opts
    }

    /// Drops every cached response (e.g. after changing language settings).
    pub fn clear_cache(&self) {
        self.inner.cache.clear();
    }

    fn img(&self) -> Img<'_> {
        Img {
            base: &self.inner.opts.image_base_url,
        }
    }

    fn region(&self) -> &str {
        self.inner
            .opts
            .region
            .as_deref()
            .filter(|r| !r.is_empty())
            .unwrap_or("US")
    }

    /// Two-letter language for image/video filters (`en-US` → `en`).
    fn lang2(&self) -> &str {
        self.inner.opts.language.split(['-', '_']).next().unwrap_or("en")
    }

    async fn get_value(
        &self,
        path: &str,
        params: &[(&str, String)],
        ttl: Duration,
    ) -> Result<Arc<Value>, MetaError> {
        let inner = &self.inner;
        let mut query: Vec<(&str, String)> = params.to_vec();
        if !query.iter().any(|(k, _)| *k == "language") {
            query.push(("language", inner.opts.language.clone()));
        }
        query.sort();
        let key = format!("{path}?{query:?}");
        if let Some(v) = inner.cache.get(&key) {
            return Ok(v);
        }
        let url = format!("{}{}", inner.opts.base_url.trim_end_matches('/'), path);
        let resp = send(
            || {
                let req = inner
                    .http
                    .get(&url)
                    .query(&query)
                    .header("Accept", "application/json");
                match &inner.auth {
                    TmdbAuth::ApiKey(k) => req.query(&[("api_key", k)]),
                    TmdbAuth::Bearer(t) => req.bearer_auth(t),
                }
            },
            Some(&inner.limiter),
            &inner.opts.retry,
        )
        .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if !status.is_success() {
            let err: wire::WError = serde_json::from_slice(&body).unwrap_or_default();
            let message = if err.status_message.is_empty() {
                String::from_utf8_lossy(&body).chars().take(200).collect()
            } else {
                err.status_message
            };
            return Err(match status {
                StatusCode::NOT_FOUND => MetaError::NotFound,
                StatusCode::UNAUTHORIZED => MetaError::Unauthorized(message),
                StatusCode::TOO_MANY_REQUESTS => MetaError::RateLimited {
                    retry_after_secs: None,
                },
                _ => MetaError::Status {
                    status: status.as_u16(),
                    message,
                },
            });
        }
        let value = Arc::new(serde_json::from_slice::<Value>(&body)?);
        inner.cache.insert(key, value.clone(), ttl);
        Ok(value)
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, String)],
        ttl: Duration,
    ) -> Result<T, MetaError> {
        let v = self.get_value(path, params, ttl).await?;
        Ok(T::deserialize(&*v)?)
    }

    fn details_ttl(&self) -> Duration {
        self.inner.opts.details_ttl
    }

    fn feed_ttl(&self) -> Duration {
        self.inner.opts.feed_ttl
    }

    fn media_params(&self, append: &str) -> Vec<(&'static str, String)> {
        vec![
            ("append_to_response", append.to_string()),
            ("include_image_language", format!("{},null", self.lang2())),
            ("include_video_language", format!("{},null", self.lang2())),
        ]
    }

    /// Movie details with release dates, credits, images, videos, keywords and recommendations.
    pub async fn movie_details(&self, id: TmdbId) -> Result<MovieMetadata, MetaError> {
        let raw: wire::WMovie = self
            .get(
                &format!("/movie/{id}"),
                &self.media_params(MOVIE_APPEND),
                self.details_ttl(),
            )
            .await?;
        Ok(raw.into_model(&self.img(), self.region()))
    }

    pub async fn collection(&self, id: TmdbId) -> Result<CollectionMetadata, MetaError> {
        let raw: wire::WCollection = self
            .get(&format!("/collection/{id}"), &[], self.details_ttl())
            .await?;
        Ok(raw.into_model(&self.img()))
    }

    /// Full series by TMDB id: details plus every season with episodes.
    pub async fn tv_details(&self, id: TmdbId) -> Result<SeriesMetadata, MetaError> {
        let raw: wire::WTv = self
            .get(
                &format!("/tv/{id}"),
                &self.media_params(TV_APPEND),
                self.details_ttl(),
            )
            .await?;
        let numbers: Vec<u32> = raw.seasons.iter().map(|s| s.season_number).collect();
        let mut seasons = Vec::with_capacity(numbers.len());
        // Seasons are fetched in batches through append_to_response to save requests.
        for chunk in numbers.chunks(MAX_APPEND) {
            let append = chunk
                .iter()
                .map(|n| format!("season/{n}"))
                .collect::<Vec<_>>()
                .join(",");
            let v = self
                .get_value(
                    &format!("/tv/{id}"),
                    &[("append_to_response", append)],
                    self.details_ttl(),
                )
                .await?;
            for n in chunk {
                if let Some(s) = v.get(format!("season/{n}")) {
                    let s = wire::WSeason::deserialize_value(s)?;
                    seasons.push(s.into_model(&self.img()));
                }
            }
        }
        Ok(raw.into_model(&self.img(), self.region(), seasons))
    }

    /// One season with its episodes.
    pub async fn season(&self, tv_id: TmdbId, season: u32) -> Result<SeasonMetadata, MetaError> {
        let raw: wire::WSeason = self
            .get(&format!("/tv/{tv_id}/season/{season}"), &[], self.details_ttl())
            .await?;
        Ok(raw.into_model(&self.img()))
    }

    /// Resolves IMDb (`tt…`) or TVDB ids to TMDB results.
    pub async fn find(&self, source: ExternalSource, id: &str) -> Result<FindResults, MetaError> {
        let src = match source {
            ExternalSource::Imdb => "imdb_id",
            ExternalSource::Tvdb => "tvdb_id",
        };
        let raw: wire::WFind = self
            .get(
                &format!("/find/{id}"),
                &[("external_source", src.into())],
                self.details_ttl(),
            )
            .await?;
        let img = self.img();
        let mut out = FindResults {
            movies: wire::hits(raw.movie_results, Some(MediaKind::Movie), &img),
            series: wire::hits(raw.tv_results, Some(MediaKind::Series), &img),
        };
        for h in out.movies.iter_mut().chain(out.series.iter_mut()) {
            match source {
                ExternalSource::Imdb => h.imdb_id = Some(id.to_string()),
                ExternalSource::Tvdb => h.tvdb_id = id.parse().ok(),
            }
        }
        Ok(out)
    }

    async fn resolve_series(&self, id: &SeriesExternalId) -> Result<TmdbId, MetaError> {
        let (source, key) = match id {
            SeriesExternalId::Tmdb(id) => return Ok(*id),
            SeriesExternalId::Tvdb(id) => (ExternalSource::Tvdb, id.to_string()),
            SeriesExternalId::Imdb(id) => (ExternalSource::Imdb, id.clone()),
        };
        self.find(source, &key)
            .await?
            .series
            .first()
            .and_then(|h| h.tmdb_id)
            .ok_or(MetaError::NotFound)
    }

    fn search_params(&self, query: &str, page: u32) -> Vec<(&'static str, String)> {
        vec![
            ("query", query.to_string()),
            ("page", page.clamp(1, 500).to_string()),
            ("include_adult", self.inner.opts.include_adult.to_string()),
        ]
    }

    pub async fn search_movies(
        &self,
        query: &str,
        year: Option<i32>,
        page: u32,
    ) -> Result<Page<SearchHit>, MetaError> {
        let mut p = self.search_params(query, page);
        if let Some(y) = year {
            p.push(("year", y.to_string()));
        }
        if let Some(r) = &self.inner.opts.region {
            p.push(("region", r.clone()));
        }
        let raw: wire::WPage<wire::WHit> = self.get("/search/movie", &p, self.feed_ttl()).await?;
        Ok(wire::page_of(raw, Some(MediaKind::Movie), &self.img()))
    }

    pub async fn search_series(
        &self,
        query: &str,
        year: Option<i32>,
        page: u32,
    ) -> Result<Page<SearchHit>, MetaError> {
        let mut p = self.search_params(query, page);
        if let Some(y) = year {
            p.push(("first_air_date_year", y.to_string()));
        }
        let raw: wire::WPage<wire::WHit> = self.get("/search/tv", &p, self.feed_ttl()).await?;
        Ok(wire::page_of(raw, Some(MediaKind::Series), &self.img()))
    }

    /// Movies, series and persons in one list.
    pub async fn search_multi(&self, query: &str, page: u32) -> Result<Page<MultiHit>, MetaError> {
        let raw: wire::WPage<wire::WHit> = self
            .get("/search/multi", &self.search_params(query, page), self.feed_ttl())
            .await?;
        let img = self.img();
        Ok(Page {
            page: raw.page.max(1),
            total_pages: raw.total_pages,
            total_results: raw.total_results,
            results: raw
                .results
                .into_iter()
                .filter_map(|h| h.into_multi(&img))
                .collect(),
        })
    }

    /// One page of a discovery feed.
    pub async fn discover_feed(&self, feed: &DiscoverFeed, page: u32) -> Result<Page<SearchHit>, MetaError> {
        use DiscoverFeed as F;
        let mut params = vec![("page", page.clamp(1, 500).to_string())];
        let region = self.inner.opts.region.clone();
        let with_region = |p: &mut Vec<(&str, String)>| {
            if let Some(r) = &region {
                p.push(("region", r.clone()));
            }
        };
        let discover = |p: &mut Vec<(&str, String)>, filter: &'static str, id: u32| {
            p.push((filter, id.to_string()));
            p.push(("sort_by", "popularity.desc".into()));
            p.push(("include_adult", self.inner.opts.include_adult.to_string()));
        };
        let (path, default_kind) = match feed {
            F::Trending { kind, window } => {
                let k = kind.map_or("all", kind_path);
                let w = match window {
                    TimeWindow::Day => "day",
                    TimeWindow::Week => "week",
                };
                (format!("/trending/{k}/{w}"), *kind)
            }
            F::Popular { kind } | F::TopRated { kind } => {
                if *kind == MediaKind::Movie {
                    with_region(&mut params);
                }
                let list = if matches!(feed, F::Popular { .. }) {
                    "popular"
                } else {
                    "top_rated"
                };
                (format!("/{}/{list}", kind_path(*kind)), Some(*kind))
            }
            F::Upcoming => {
                with_region(&mut params);
                ("/movie/upcoming".into(), Some(MediaKind::Movie))
            }
            F::NowPlaying => {
                with_region(&mut params);
                ("/movie/now_playing".into(), Some(MediaKind::Movie))
            }
            F::OnTheAir => ("/tv/on_the_air".into(), Some(MediaKind::Series)),
            F::AiringToday => ("/tv/airing_today".into(), Some(MediaKind::Series)),
            F::Genre { kind, genre_id } => {
                discover(&mut params, "with_genres", *genre_id);
                (format!("/discover/{}", kind_path(*kind)), Some(*kind))
            }
            F::Network { network_id } => {
                discover(&mut params, "with_networks", *network_id);
                ("/discover/tv".into(), Some(MediaKind::Series))
            }
            F::Studio { company_id } => {
                discover(&mut params, "with_companies", *company_id);
                ("/discover/movie".into(), Some(MediaKind::Movie))
            }
            F::Keyword { kind, keyword_id } => {
                discover(&mut params, "with_keywords", *keyword_id);
                (format!("/discover/{}", kind_path(*kind)), Some(*kind))
            }
        };
        let raw: wire::WPage<wire::WHit> = self.get(&path, &params, self.feed_ttl()).await?;
        Ok(wire::page_of(raw, default_kind, &self.img()))
    }

    /// Paged recommendations for a movie or series.
    pub async fn recommendations(
        &self,
        kind: MediaKind,
        id: TmdbId,
        page: u32,
    ) -> Result<Page<SearchHit>, MetaError> {
        self.related(kind, id, "recommendations", page).await
    }

    /// Paged "similar" titles for a movie or series.
    pub async fn similar(
        &self,
        kind: MediaKind,
        id: TmdbId,
        page: u32,
    ) -> Result<Page<SearchHit>, MetaError> {
        self.related(kind, id, "similar", page).await
    }

    async fn related(
        &self,
        kind: MediaKind,
        id: TmdbId,
        what: &str,
        page: u32,
    ) -> Result<Page<SearchHit>, MetaError> {
        let path = format!("/{}/{id}/{what}", kind_path(kind));
        let raw: wire::WPage<wire::WHit> = self
            .get(
                &path,
                &[("page", page.clamp(1, 500).to_string())],
                self.feed_ttl(),
            )
            .await?;
        Ok(wire::page_of(raw, Some(kind), &self.img()))
    }

    /// Official genre list for movies or series.
    pub async fn genres(&self, kind: MediaKind) -> Result<Vec<Genre>, MetaError> {
        let raw: wire::WGenres = self
            .get(
                &format!("/genre/{}/list", kind_path(kind)),
                &[],
                self.details_ttl(),
            )
            .await?;
        Ok(raw.genres)
    }

    /// Basic person details.
    pub async fn person(&self, id: TmdbId) -> Result<Person, MetaError> {
        let raw: wire::WPerson = self
            .get(&format!("/person/{id}"), &[], self.details_ttl())
            .await?;
        Ok(raw.into_model(&self.img()))
    }
}

impl wire::WSeason {
    fn deserialize_value(v: &Value) -> Result<Self, MetaError> {
        Ok(<Self as serde::Deserialize>::deserialize(v)?)
    }
}

#[async_trait]
impl MetadataProvider for Tmdb {
    fn name(&self) -> &'static str {
        "tmdb"
    }

    async fn movie(&self, id: TmdbId) -> Result<MovieMetadata, MetaError> {
        self.movie_details(id).await
    }

    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        let tmdb_id = self.resolve_series(&id).await?;
        let mut s = self.tv_details(tmdb_id).await?;
        if let SeriesExternalId::Tvdb(t) = id {
            s.tvdb_id.get_or_insert(t);
            s.external_ids.tvdb.get_or_insert(t);
        }
        Ok(s)
    }

    async fn search(&self, q: &str, kind: MediaKind) -> Result<Vec<SearchHit>, MetaError> {
        let page = match kind {
            MediaKind::Movie => self.search_movies(q, None, 1).await?,
            MediaKind::Series => self.search_series(q, None, 1).await?,
        };
        Ok(page.results)
    }

    async fn discover(&self, feed: DiscoverFeed, page: u32) -> Result<Page<SearchHit>, MetaError> {
        self.discover_feed(&feed, page).await
    }
}
