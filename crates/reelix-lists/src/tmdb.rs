//! TMDB lists: user lists, collections, popular/top-rated/upcoming/now-playing feeds, person
//! credits, and company/keyword discover queries. Uses a tiny local v3 client (API key or v4
//! read-access token as bearer).

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;

use crate::common::{self, KindFilter, Settings, year_from_date};
use crate::{BuildContext, ImportList, ListError, ListItem, dedupe};

pub const IMPLEMENTATION: &str = "tmdb";

/// What a TMDB list instance pulls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmdbListType {
    List,
    Collection,
    PopularMovies,
    TopRatedMovies,
    Upcoming,
    NowPlaying,
    PopularSeries,
    TopRatedSeries,
    Person,
    Company,
    Keyword,
}

const LIST_TYPES: &[(&str, &str, TmdbListType)] = &[
    ("list", "List (by id)", TmdbListType::List),
    ("collection", "Collection", TmdbListType::Collection),
    ("popular_movies", "Popular movies", TmdbListType::PopularMovies),
    (
        "top_rated_movies",
        "Top rated movies",
        TmdbListType::TopRatedMovies,
    ),
    ("upcoming", "Upcoming movies", TmdbListType::Upcoming),
    ("now_playing", "Now playing", TmdbListType::NowPlaying),
    ("popular_series", "Popular series", TmdbListType::PopularSeries),
    (
        "top_rated_series",
        "Top rated series",
        TmdbListType::TopRatedSeries,
    ),
    ("person", "Person credits", TmdbListType::Person),
    ("company", "Company", TmdbListType::Company),
    ("keyword", "Keyword", TmdbListType::Keyword),
];

impl TmdbListType {
    fn parse(s: &str) -> Option<Self> {
        LIST_TYPES.iter().find(|(v, _, _)| *v == s).map(|(_, _, t)| *t)
    }

    fn needs_id(self) -> bool {
        matches!(
            self,
            Self::List | Self::Collection | Self::Person | Self::Company | Self::Keyword
        )
    }
}

/// Which credits of a person to include.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonCredits {
    Cast,
    Crew,
    Both,
}

#[derive(Debug, Clone)]
enum Auth {
    ApiKey(String),
    Bearer(String),
}

pub fn schema() -> ProviderSchema {
    let types: Vec<(&str, &str)> = LIST_TYPES.iter().map(|(v, l, _)| (*v, *l)).collect();
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "TMDB".into(),
        description: Some(
            "Lists, collections, popular feeds, people, companies and keywords from TMDB.".into(),
        ),
        info_url: Some("https://developer.themoviedb.org/docs".into()),
        fields: vec![
            FieldSchema::new("api_key", "API key or read access token", FieldKind::Secret)
                .required()
                .help("A v3 API key or a v4 read access token."),
            FieldSchema::new("list_type", "List type", common::select(&types))
                .required()
                .default_value("list"),
            FieldSchema::new(
                "list_id",
                "List / collection / person / company / keyword id",
                FieldKind::Text,
            )
            .help("Required for list, collection, person, company and keyword types."),
            FieldSchema::new(
                "media",
                "Media (company/keyword)",
                common::select(&[("movies", "Movies"), ("series", "Series")]),
            )
            .default_value("movies"),
            FieldSchema::new(
                "person_credits",
                "Person credits",
                common::select(&[("cast", "Cast"), ("crew", "Crew"), ("both", "Cast and crew")]),
            )
            .default_value("cast"),
            FieldSchema::new("filter", "Include", common::select(KindFilter::OPTIONS)).default_value("both"),
            FieldSchema::new("max_pages", "Max pages", FieldKind::Number)
                .default_value(5)
                .help("Pages of 20 results fetched for paged feeds.")
                .advanced(),
            FieldSchema::new("language", "Language", FieldKind::Text)
                .default_value("en-US")
                .advanced(),
            common::base_url_field("https://api.themoviedb.org/3"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured TMDB list.
#[derive(Debug, Clone)]
pub struct TmdbList {
    http: reqwest::Client,
    base_url: String,
    auth: Auth,
    list_type: TmdbListType,
    id: Option<String>,
    series_discover: bool,
    person_credits: PersonCredits,
    filter: KindFilter,
    max_pages: u32,
    language: String,
}

#[derive(Debug, Deserialize)]
struct Entry {
    id: u64,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
}

impl Entry {
    fn into_item(self, default_kind: MediaKind) -> Option<ListItem> {
        let kind = match self.media_type.as_deref() {
            Some("movie") => MediaKind::Movie,
            Some("tv") => MediaKind::Series,
            Some(_) => return None, // e.g. "person" in mixed lists
            None => default_kind,
        };
        let date = self.release_date.as_deref().or(self.first_air_date.as_deref());
        Some(ListItem {
            year: year_from_date(date),
            tmdb_id: Some(self.id),
            ..ListItem::new(kind, self.title.or(self.name).unwrap_or_default())
        })
    }
}

#[derive(Debug, Deserialize)]
struct Paged {
    #[serde(default)]
    results: Vec<Entry>,
    /// `/list/{id}` uses `items` instead of `results`.
    #[serde(default)]
    items: Vec<Entry>,
    #[serde(default)]
    total_pages: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct Collection {
    #[serde(default)]
    parts: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Credits {
    #[serde(default)]
    cast: Vec<Entry>,
    #[serde(default)]
    crew: Vec<Entry>,
}

impl TmdbList {
    pub fn from_settings(settings: &serde_json::Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        let key = s.req("api_key")?;
        // v4 read tokens are JWTs (long, dotted); v3 keys are 32 hex chars.
        let auth = if key.len() > 40 && key.contains('.') {
            Auth::Bearer(key)
        } else {
            Auth::ApiKey(key)
        };
        let lt = s.req("list_type")?;
        let list_type = TmdbListType::parse(&lt)
            .ok_or_else(|| ListError::InvalidSettings(format!("unknown TMDB list type {lt:?}")))?;
        let id = s.str("list_id");
        if list_type.needs_id() && id.is_none() {
            return Err(ListError::InvalidSettings(format!(
                "list_id is required for {lt}"
            )));
        }
        if list_type != TmdbListType::List
            && let Some(id) = &id
            && list_type.needs_id()
            && !id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(ListError::InvalidSettings("list_id must be numeric".into()));
        }
        let person_credits = match s.str("person_credits").as_deref() {
            Some("crew") => PersonCredits::Crew,
            Some("both") => PersonCredits::Both,
            _ => PersonCredits::Cast,
        };
        Ok(Self {
            http: ctx.http.clone(),
            base_url: s.base_url(&ctx.base_urls.tmdb),
            auth,
            list_type,
            id,
            series_discover: s.str("media").as_deref() == Some("series"),
            person_credits,
            filter: KindFilter::parse(s.str("filter"))?,
            max_pages: s.u64("max_pages").unwrap_or(5).clamp(1, 50) as u32,
            language: s.str("language").unwrap_or_else(|| "en-US".into()),
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self
            .http
            .get(format!("{}{path}", self.base_url))
            .query(&[("language", &self.language)]);
        match &self.auth {
            Auth::ApiKey(k) => req.query(&[("api_key", k)]),
            Auth::Bearer(t) => req.bearer_auth(t),
        }
    }

    async fn paged(
        &self,
        path: &str,
        extra: &[(&str, &str)],
        kind: MediaKind,
        max_pages: u32,
    ) -> Result<Vec<ListItem>, ListError> {
        let mut out = Vec::new();
        for page in 1..=max_pages {
            let p: Paged = common::get_json(self.get(path).query(extra).query(&[("page", page)])).await?;
            let n = p.results.len() + p.items.len();
            out.extend(
                p.results
                    .into_iter()
                    .chain(p.items)
                    .filter_map(|e| e.into_item(kind)),
            );
            if n == 0 || page >= p.total_pages.unwrap_or(1) {
                break;
            }
        }
        Ok(out)
    }

    async fn collect(&self, max_pages: u32) -> Result<Vec<ListItem>, ListError> {
        use MediaKind::{Movie, Series};
        use TmdbListType as T;
        let id = self.id.as_deref().unwrap_or_default();
        let discover_kind = if self.series_discover { Series } else { Movie };
        let discover_path = if self.series_discover {
            "/discover/tv"
        } else {
            "/discover/movie"
        };
        let items = match self.list_type {
            T::List => self.paged(&format!("/list/{id}"), &[], Movie, max_pages).await?,
            T::Collection => {
                let c: Collection = common::get_json(self.get(&format!("/collection/{id}"))).await?;
                c.parts.into_iter().filter_map(|e| e.into_item(Movie)).collect()
            }
            T::PopularMovies => self.paged("/movie/popular", &[], Movie, max_pages).await?,
            T::TopRatedMovies => self.paged("/movie/top_rated", &[], Movie, max_pages).await?,
            T::Upcoming => self.paged("/movie/upcoming", &[], Movie, max_pages).await?,
            T::NowPlaying => self.paged("/movie/now_playing", &[], Movie, max_pages).await?,
            T::PopularSeries => self.paged("/tv/popular", &[], Series, max_pages).await?,
            T::TopRatedSeries => self.paged("/tv/top_rated", &[], Series, max_pages).await?,
            T::Person => {
                let c: Credits =
                    common::get_json(self.get(&format!("/person/{id}/combined_credits"))).await?;
                let (cast, crew) = match self.person_credits {
                    PersonCredits::Cast => (c.cast, Vec::new()),
                    PersonCredits::Crew => (Vec::new(), c.crew),
                    PersonCredits::Both => (c.cast, c.crew),
                };
                cast.into_iter()
                    .chain(crew)
                    .filter_map(|e| e.into_item(Movie))
                    .collect()
            }
            T::Company => {
                self.paged(discover_path, &[("with_companies", id)], discover_kind, max_pages)
                    .await?
            }
            T::Keyword => {
                self.paged(discover_path, &[("with_keywords", id)], discover_kind, max_pages)
                    .await?
            }
        };
        Ok(dedupe(
            items.into_iter().filter(|i| self.filter.allows(i.kind)).collect(),
        ))
    }
}

#[async_trait]
impl ImportList for TmdbList {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        self.collect(self.max_pages).await
    }

    async fn test(&self) -> Result<(), ListError> {
        self.collect(1).await.map(|_| ())
    }
}
