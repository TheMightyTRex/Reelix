//! Plex account watchlist via the discover service (`/library/sections/watchlist/all`).

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::Value;

use crate::common::{self, KindFilter, Settings};
use crate::{BuildContext, ImportList, ListError, ListItem, dedupe};

pub const IMPLEMENTATION: &str = "plex_watchlist";
const PAGE_SIZE: u64 = 100;
const MAX_PAGES: u64 = 50;

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Plex Watchlist".into(),
        description: Some("The watchlist of a Plex account.".into()),
        info_url: Some("https://watch.plex.tv/".into()),
        fields: vec![
            FieldSchema::new("token", "Plex token", FieldKind::Secret).required(),
            FieldSchema::new("filter", "Include", common::select(KindFilter::OPTIONS)).default_value("both"),
            FieldSchema::new("client_identifier", "Client identifier", FieldKind::Text)
                .default_value("reelix")
                .advanced(),
            common::base_url_field("https://discover.provider.plex.tv"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured Plex watchlist.
#[derive(Debug, Clone)]
pub struct PlexWatchlist {
    http: reqwest::Client,
    base_url: String,
    token: String,
    client_id: String,
    filter: KindFilter,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(rename = "MediaContainer")]
    container: Container,
}

#[derive(Debug, Deserialize)]
struct Container {
    #[serde(default, rename = "totalSize")]
    total_size: Option<u64>,
    #[serde(default, rename = "Metadata")]
    metadata: Vec<Metadata>,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    #[serde(default)]
    title: String,
    #[serde(default)]
    year: Option<i32>,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default, rename = "ratingKey")]
    rating_key: Option<String>,
    #[serde(default, rename = "Guid")]
    guids: Vec<Guid>,
}

#[derive(Debug, Deserialize)]
struct Guid {
    id: String,
}

impl Metadata {
    fn kind(&self) -> Option<MediaKind> {
        match self.kind.as_str() {
            "movie" => Some(MediaKind::Movie),
            "show" => Some(MediaKind::Series),
            _ => None,
        }
    }

    fn into_item(self, kind: MediaKind) -> ListItem {
        let mut item = ListItem {
            year: self.year,
            ..ListItem::new(kind, self.title)
        };
        for g in self.guids {
            // Guid ids look like `imdb://tt0133093`, `tmdb://603`, `tvdb://81189`.
            match g.id.split_once("://") {
                Some(("imdb", v)) => item.imdb_id = common::imdb_id(Some(v.to_string())),
                Some(("tmdb", v)) => item.tmdb_id = v.parse().ok(),
                Some(("tvdb", v)) => item.tvdb_id = v.parse().ok(),
                _ => {}
            }
        }
        item
    }
}

impl PlexWatchlist {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        Ok(Self {
            http: ctx.http.clone(),
            base_url: s.base_url(&ctx.base_urls.plex_discover),
            token: s.req("token")?,
            client_id: s.str("client_identifier").unwrap_or_else(|| "reelix".into()),
            filter: KindFilter::parse(s.str("filter"))?,
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{path}", self.base_url))
            .header(reqwest::header::ACCEPT, "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", &self.client_id)
            .header("X-Plex-Product", "Reelix")
    }

    async fn collect(&self, max_pages: u64) -> Result<Vec<ListItem>, ListError> {
        let mut out = Vec::new();
        for page in 0..max_pages {
            let start = page * PAGE_SIZE;
            let req = self.get("/library/sections/watchlist/all").query(&[
                ("includeGuids", "1".to_string()),
                ("X-Plex-Container-Start", start.to_string()),
                ("X-Plex-Container-Size", PAGE_SIZE.to_string()),
            ]);
            let env: Envelope = common::get_json(req).await?;
            let n = env.container.metadata.len() as u64;
            for m in env.container.metadata {
                let Some(kind) = m.kind() else { continue };
                if !self.filter.allows(kind) {
                    continue;
                }
                let m = match (&m.rating_key, m.guids.is_empty()) {
                    (Some(key), true) => self.details(key).await?.unwrap_or(m),
                    _ => m,
                };
                out.push(m.into_item(kind));
            }
            if n < PAGE_SIZE || env.container.total_size.is_some_and(|t| start + n >= t) {
                break;
            }
        }
        Ok(dedupe(out))
    }

    /// Item details, used when the watchlist entry carries no external guids.
    async fn details(&self, rating_key: &str) -> Result<Option<Metadata>, ListError> {
        let req = self
            .get(&format!("/library/metadata/{rating_key}"))
            .query(&[("includeGuids", "1")]);
        let env: Envelope = common::get_json(req).await?;
        Ok(env.container.metadata.into_iter().next())
    }
}

#[async_trait]
impl ImportList for PlexWatchlist {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        self.collect(MAX_PAGES).await
    }

    async fn test(&self) -> Result<(), ListError> {
        let req = self
            .get("/library/sections/watchlist/all")
            .query(&[("X-Plex-Container-Start", "0"), ("X-Plex-Container-Size", "1")]);
        common::get_json::<Envelope>(req).await.map(|_| ())
    }
}
