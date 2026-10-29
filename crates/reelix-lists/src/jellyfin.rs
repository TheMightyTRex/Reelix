//! Jellyfin / Emby favourites of one user (`/Users/{id}/Items?Filters=IsFavorite`).

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::common::{self, KindFilter, Settings};
use crate::{BuildContext, ImportList, ListError, ListItem, dedupe};

const PAGE_SIZE: u64 = 200;
const MAX_PAGES: u64 = 50;

/// Server flavour; both share the API, only auth headers and names differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Jellyfin,
    Emby,
}

impl Flavor {
    pub fn implementation(self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
            Self::Emby => "emby",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Jellyfin => "Jellyfin",
            Self::Emby => "Emby",
        }
    }
}

pub fn schema(flavor: Flavor) -> ProviderSchema {
    let name = flavor.name();
    ProviderSchema {
        implementation: flavor.implementation().into(),
        name: format!("{name} Favourites"),
        description: Some(format!("Movies and series a {name} user marked as favourite.")),
        info_url: None,
        fields: vec![
            FieldSchema::new("url", "Server URL", FieldKind::Url)
                .required()
                .placeholder("http://localhost:8096"),
            FieldSchema::new("api_key", "API key", FieldKind::Secret).required(),
            FieldSchema::new("user", "User", FieldKind::Text)
                .required()
                .help("User id or user name (names are resolved with the API key)."),
            FieldSchema::new("filter", "Include", common::select(KindFilter::OPTIONS)).default_value("both"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured favourites list.
#[derive(Debug, Clone)]
pub struct Favourites {
    flavor: Flavor,
    http: reqwest::Client,
    url: String,
    api_key: String,
    user: String,
    filter: KindFilter,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemsPage {
    #[serde(default)]
    items: Vec<Item>,
    #[serde(default)]
    total_record_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    #[serde(default)]
    name: String,
    #[serde(default)]
    production_year: Option<i32>,
    #[serde(default, rename = "Type")]
    kind: String,
    #[serde(default)]
    provider_ids: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct User {
    id: String,
    name: String,
}

impl Item {
    fn provider(&self, key: &str) -> Option<String> {
        self.provider_ids
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| {
                common::value_str(Some(v)).or_else(|| common::value_u64(Some(v)).map(|n| n.to_string()))
            })
    }

    fn into_item(self) -> Option<ListItem> {
        let kind = match self.kind.as_str() {
            "Movie" => MediaKind::Movie,
            "Series" => MediaKind::Series,
            _ => return None,
        };
        Some(ListItem {
            year: self.production_year,
            tmdb_id: self.provider("Tmdb").and_then(|v| v.parse().ok()),
            imdb_id: common::imdb_id(self.provider("Imdb")),
            tvdb_id: self.provider("Tvdb").and_then(|v| v.parse().ok()),
            ..ListItem::new(kind, self.name)
        })
    }
}

/// Jellyfin/Emby user ids are 32 hex chars (optionally dashed).
fn looks_like_id(s: &str) -> bool {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Favourites {
    pub fn from_settings(flavor: Flavor, settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(flavor), settings)?;
        Ok(Self {
            flavor,
            http: ctx.http.clone(),
            url: s.server_url("url")?,
            api_key: s.req("api_key")?,
            user: s.req("user")?,
            filter: KindFilter::parse(s.str("filter"))?,
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self
            .http
            .get(format!("{}{path}", self.url))
            .header(reqwest::header::ACCEPT, "application/json");
        match self.flavor {
            Flavor::Jellyfin => req.header(
                reqwest::header::AUTHORIZATION,
                format!(
                    r#"MediaBrowser Client="Reelix", Device="Reelix", DeviceId="reelix-lists", Version="{}", Token="{}""#,
                    env!("CARGO_PKG_VERSION"),
                    self.api_key
                ),
            ),
            Flavor::Emby => req.header("X-Emby-Token", &self.api_key),
        }
    }

    async fn user_id(&self) -> Result<String, ListError> {
        if looks_like_id(&self.user) {
            return Ok(self.user.clone());
        }
        let users: Vec<User> = common::get_json(self.get("/Users")).await?;
        users
            .into_iter()
            .find(|u| u.name.eq_ignore_ascii_case(&self.user))
            .map(|u| u.id)
            .ok_or_else(|| ListError::NotFound(format!("user {}", self.user)))
    }

    fn item_types(&self) -> &'static str {
        match self.filter {
            KindFilter::Movies => "Movie",
            KindFilter::Series => "Series",
            KindFilter::Both => "Movie,Series",
        }
    }

    async fn collect(&self, max_pages: u64) -> Result<Vec<ListItem>, ListError> {
        let path = format!("/Users/{}/Items", self.user_id().await?);
        let mut out = Vec::new();
        for page in 0..max_pages {
            let start = page * PAGE_SIZE;
            let req = self.get(&path).query(&[
                ("Recursive", "true".to_string()),
                ("Filters", "IsFavorite".to_string()),
                ("IncludeItemTypes", self.item_types().to_string()),
                ("Fields", "ProviderIds,ProductionYear".to_string()),
                ("StartIndex", start.to_string()),
                ("Limit", PAGE_SIZE.to_string()),
            ]);
            let p: ItemsPage = common::get_json(req).await?;
            let n = p.items.len() as u64;
            out.extend(p.items.into_iter().filter_map(Item::into_item));
            if n < PAGE_SIZE || p.total_record_count.is_some_and(|t| start + n >= t) {
                break;
            }
        }
        Ok(dedupe(
            out.into_iter().filter(|i| self.filter.allows(i.kind)).collect(),
        ))
    }
}

#[async_trait]
impl ImportList for Favourites {
    fn implementation(&self) -> &'static str {
        self.flavor.implementation()
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        self.collect(MAX_PAGES).await
    }

    async fn test(&self) -> Result<(), ListError> {
        self.collect(1).await.map(|_| ())
    }
}
