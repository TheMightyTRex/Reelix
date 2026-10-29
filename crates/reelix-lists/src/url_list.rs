//! Generic URL list: a JSON array fetched from any URL.
//!
//! Accepted element shapes (mixable):
//! - a number or numeric string → id of the configured `id_type` (TMDB by default),
//! - a string `tt…` → IMDb id,
//! - an object with any of `tmdb_id`/`tmdbId`/`id`, `imdb_id`/`imdbId`, `tvdb_id`/`tvdbId`,
//!   `title`, `year`, and `kind`/`type`/`media_type` (`movie`, `series`/`show`/`tv`).
//!
//! A top-level object with an `items` array is also accepted.

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::Value;

use crate::common::{self, Settings};
use crate::{BuildContext, ImportList, ListError, ListItem, dedupe};

pub const IMPLEMENTATION: &str = "url_list";

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "URL list".into(),
        description: Some("A JSON array of ids or objects served from any URL.".into()),
        info_url: None,
        fields: vec![
            FieldSchema::new("url", "URL", FieldKind::Url)
                .required()
                .placeholder("https://example.com/list.json"),
            FieldSchema::new(
                "kind",
                "Default media kind",
                common::select(&[("movie", "Movie"), ("series", "Series")]),
            )
            .default_value("movie"),
            FieldSchema::new(
                "id_type",
                "Bare numeric ids are",
                common::select(&[("tmdb", "TMDB ids"), ("tvdb", "TVDB ids")]),
            )
            .default_value("tmdb"),
            FieldSchema::new("bearer_token", "Bearer token", FieldKind::Secret)
                .help("Optional Authorization: Bearer token.")
                .advanced(),
            common::sync_interval_field(),
        ],
    }
}

/// A configured URL list.
#[derive(Debug, Clone)]
pub struct UrlList {
    http: reqwest::Client,
    url: String,
    kind: MediaKind,
    tvdb_numbers: bool,
    bearer: Option<String>,
}

fn parse_kind(s: &str) -> Option<MediaKind> {
    match s.to_lowercase().as_str() {
        "movie" | "movies" => Some(MediaKind::Movie),
        "series" | "show" | "shows" | "tv" => Some(MediaKind::Series),
        _ => None,
    }
}

impl UrlList {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        let kind = parse_kind(&s.str("kind").unwrap_or_else(|| "movie".into()))
            .ok_or_else(|| ListError::InvalidSettings("kind must be movie or series".into()))?;
        Ok(Self {
            http: ctx.http.clone(),
            url: s.server_url("url")?,
            kind,
            tvdb_numbers: s.str("id_type").as_deref() == Some("tvdb"),
            bearer: s.str("bearer_token"),
        })
    }

    fn item_for_number(&self, n: u64) -> ListItem {
        let mut item = ListItem::new(self.kind, "");
        if self.tvdb_numbers {
            item.tvdb_id = Some(n);
        } else {
            item.tmdb_id = Some(n);
        }
        item
    }

    fn parse_element(&self, v: &Value) -> Option<ListItem> {
        match v {
            Value::Number(n) => n.as_u64().map(|n| self.item_for_number(n)),
            Value::String(s) => {
                let s = s.trim();
                if let Some(id) = common::imdb_id(Some(s.to_string())) {
                    Some(ListItem {
                        imdb_id: Some(id),
                        ..ListItem::new(self.kind, "")
                    })
                } else {
                    s.parse().ok().map(|n| self.item_for_number(n))
                }
            }
            Value::Object(o) => {
                let pick = |keys: &[&str]| keys.iter().find_map(|k| o.get(*k));
                let kind = pick(&["kind", "type", "media_type"])
                    .and_then(Value::as_str)
                    .and_then(parse_kind)
                    .unwrap_or(self.kind);
                let item = ListItem {
                    year: common::value_year(pick(&["year", "release_year"])),
                    tmdb_id: common::value_u64(pick(&["tmdb_id", "tmdbId", "id"])),
                    imdb_id: common::imdb_id(common::value_str(pick(&["imdb_id", "imdbId"]))),
                    tvdb_id: common::value_u64(pick(&["tvdb_id", "tvdbId"])),
                    ..ListItem::new(
                        kind,
                        common::value_str(pick(&["title", "name"])).unwrap_or_default(),
                    )
                };
                item.has_id().then_some(item)
            }
            _ => None,
        }
    }
}

#[async_trait]
impl ImportList for UrlList {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        let mut req = self
            .http
            .get(&self.url)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(t) = &self.bearer {
            req = req.bearer_auth(t);
        }
        let v: Value = common::get_json(req).await?;
        let arr = match &v {
            Value::Array(a) => a,
            Value::Object(o) => o
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| ListError::Parse("expected a JSON array or {\"items\": [...]}".into()))?,
            _ => return Err(ListError::Parse("expected a JSON array".into())),
        };
        Ok(dedupe(arr.iter().filter_map(|e| self.parse_element(e)).collect()))
    }
}
