//! MDBList list items (`/lists/{id}/items` or `/lists/{user}/{slug}/items`) with an API key.

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::Value;

use crate::common::{self, KindFilter, Settings};
use crate::{BuildContext, ImportList, ListError, ListItem, dedupe};

pub const IMPLEMENTATION: &str = "mdblist";
const PAGE_SIZE: usize = 1000;
const MAX_PAGES: usize = 20;

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "MDBList".into(),
        description: Some("Items of an MDBList list.".into()),
        info_url: Some("https://mdblist.com/".into()),
        fields: vec![
            FieldSchema::new("api_key", "API key", FieldKind::Secret).required(),
            FieldSchema::new("list_id", "List", FieldKind::Text)
                .required()
                .placeholder("12345, user/list-slug or https://mdblist.com/lists/user/list-slug"),
            FieldSchema::new("filter", "Include", common::select(KindFilter::OPTIONS)).default_value("both"),
            common::base_url_field("https://api.mdblist.com"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured MDBList list.
#[derive(Debug, Clone)]
pub struct MdbList {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    /// Path segment(s) after `/lists/`: `12345` or `user/slug`.
    list: String,
    filter: KindFilter,
}

/// Normalises `12345`, `user/slug`, or a mdblist.com URL into the `/lists/` path segment(s).
fn list_path(raw: &str) -> Option<String> {
    let s = raw.trim();
    let s = s.split_once("/lists/").map_or(s, |(_, rest)| rest);
    let parts: Vec<&str> = s
        .split(['/', '?', '#'])
        .take_while(|p| !p.is_empty())
        .take(2)
        .collect();
    match parts.as_slice() {
        [id] if id.bytes().all(|b| b.is_ascii_digit()) => Some((*id).to_string()),
        [user, slug] => Some(format!("{user}/{slug}")),
        _ => None,
    }
}

fn parse_item(v: &Value, default_kind: MediaKind) -> Option<ListItem> {
    let kind = match v.get("mediatype").and_then(Value::as_str) {
        Some("show" | "tv" | "series") => MediaKind::Series,
        Some("movie") => MediaKind::Movie,
        Some(_) => return None,
        None => default_kind,
    };
    let item = ListItem {
        year: common::value_year(v.get("release_year").or(v.get("year"))),
        // MDBList's `id` is the TMDB id.
        tmdb_id: common::value_u64(v.get("tmdb_id").or(v.get("id"))),
        imdb_id: common::imdb_id(common::value_str(v.get("imdb_id"))),
        tvdb_id: common::value_u64(v.get("tvdb_id")),
        ..ListItem::new(kind, common::value_str(v.get("title")).unwrap_or_default())
    };
    (item.has_id() || !item.title.is_empty()).then_some(item)
}

/// Accepts both response shapes: a flat array, or `{ "movies": [...], "shows": [...] }`.
fn parse_page(v: &Value) -> Result<Vec<ListItem>, ListError> {
    match v {
        Value::Array(a) => Ok(a.iter().filter_map(|i| parse_item(i, MediaKind::Movie)).collect()),
        Value::Object(o) => {
            if let Some(err) = o.get("error").and_then(Value::as_str) {
                return Err(if err.to_lowercase().contains("api") {
                    ListError::Unauthorized
                } else {
                    ListError::Parse(err.into())
                });
            }
            let movies = o.get("movies").and_then(Value::as_array).into_iter().flatten();
            let shows = o.get("shows").and_then(Value::as_array).into_iter().flatten();
            Ok(movies
                .filter_map(|i| parse_item(i, MediaKind::Movie))
                .chain(shows.filter_map(|i| parse_item(i, MediaKind::Series)))
                .collect())
        }
        _ => Err(ListError::Parse("unexpected MDBList response".into())),
    }
}

impl MdbList {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        let raw = s.req("list_id")?;
        let list = list_path(&raw)
            .ok_or_else(|| ListError::InvalidSettings(format!("cannot read an MDBList list from {raw:?}")))?;
        Ok(Self {
            http: ctx.http.clone(),
            base_url: s.base_url(&ctx.base_urls.mdblist),
            api_key: s.req("api_key")?,
            list,
            filter: KindFilter::parse(s.str("filter"))?,
        })
    }

    async fn collect(&self, max_pages: usize) -> Result<Vec<ListItem>, ListError> {
        let url = format!("{}/lists/{}/items", self.base_url, self.list);
        let mut out = Vec::new();
        for page in 0..max_pages {
            let req = self.http.get(&url).query(&[
                ("apikey", self.api_key.clone()),
                ("limit", PAGE_SIZE.to_string()),
                ("offset", (page * PAGE_SIZE).to_string()),
            ]);
            let v: Value = common::get_json(req).await?;
            let items = parse_page(&v)?;
            let n = items.len();
            out.extend(items);
            if n < PAGE_SIZE {
                break;
            }
        }
        Ok(dedupe(
            out.into_iter().filter(|i| self.filter.allows(i.kind)).collect(),
        ))
    }
}

#[async_trait]
impl ImportList for MdbList {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        self.collect(MAX_PAGES).await
    }

    async fn test(&self) -> Result<(), ListError> {
        self.collect(1).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_path_forms() {
        assert_eq!(list_path("12345").as_deref(), Some("12345"));
        assert_eq!(list_path("alice/top-scifi").as_deref(), Some("alice/top-scifi"));
        assert_eq!(
            list_path("https://mdblist.com/lists/alice/top-scifi?x=1").as_deref(),
            Some("alice/top-scifi")
        );
        assert_eq!(list_path("nonsense"), None);
    }
}
