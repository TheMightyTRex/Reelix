//! Another Reelix instance as a list source.
//!
//! Minimal contract this adapter relies on (authenticated with the `x-api-key` header):
//! - `GET /api/v1/system/status` → any 2xx (used by `test`),
//! - `GET /api/v1/movies?page=N&page_size=M` and `GET /api/v1/series?page=N&page_size=M` →
//!   either a JSON array of records or `{ "items": [...], "total": n }`; records carry
//!   `title`, `year`, `tmdb_id`, `imdb_id`, `tvdb_id` (camelCase aliases accepted). Unpaged servers
//!   that return a plain array are read in one request.

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::Value;

use crate::common::{self, KindFilter, Settings};
use crate::{BuildContext, ImportList, ListError, ListItem, dedupe};

pub const IMPLEMENTATION: &str = "reelix";
const PAGE_SIZE: usize = 250;
const MAX_PAGES: usize = 200;

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Reelix".into(),
        description: Some("Movies and series of another Reelix instance.".into()),
        info_url: None,
        fields: vec![
            FieldSchema::new("url", "Instance URL", FieldKind::Url)
                .required()
                .placeholder("http://reelix:7878"),
            FieldSchema::new("api_key", "API key", FieldKind::Secret).required(),
            FieldSchema::new("filter", "Include", common::select(KindFilter::OPTIONS)).default_value("both"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured remote Reelix instance.
#[derive(Debug, Clone)]
pub struct ReelixList {
    http: reqwest::Client,
    url: String,
    api_key: String,
    filter: KindFilter,
}

fn parse_record(v: &Value, kind: MediaKind) -> Option<ListItem> {
    let o = v.as_object()?;
    let pick = |keys: &[&str]| keys.iter().find_map(|k| o.get(*k));
    let item = ListItem {
        year: common::value_year(pick(&["year"])),
        tmdb_id: common::value_u64(pick(&["tmdb_id", "tmdbId"])),
        imdb_id: common::imdb_id(common::value_str(pick(&["imdb_id", "imdbId"]))),
        tvdb_id: common::value_u64(pick(&["tvdb_id", "tvdbId"])),
        ..ListItem::new(kind, common::value_str(pick(&["title"])).unwrap_or_default())
    };
    item.has_id().then_some(item)
}

impl ReelixList {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        Ok(Self {
            http: ctx.http.clone(),
            url: s.server_url("url")?,
            api_key: s.req("api_key")?,
            filter: KindFilter::parse(s.str("filter"))?,
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}/api/v1{path}", self.url))
            .header("x-api-key", &self.api_key)
    }

    async fn collect(&self, path: &str, kind: MediaKind) -> Result<Vec<ListItem>, ListError> {
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let v: Value =
                common::get_json(self.get(path).query(&[("page", page), ("page_size", PAGE_SIZE)])).await?;
            let (rows, total) = match &v {
                // A plain array means the server doesn't paginate.
                Value::Array(a) => {
                    out.extend(a.iter().filter_map(|r| parse_record(r, kind)));
                    break;
                }
                Value::Object(o) => (
                    o.get("items")
                        .or(o.get("records"))
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    common::value_u64(o.get("total").or(o.get("totalRecords"))),
                ),
                _ => return Err(ListError::Parse("unexpected Reelix response".into())),
            };
            let n = rows.len();
            out.extend(rows.iter().filter_map(|r| parse_record(r, kind)));
            let seen = (page - 1) * PAGE_SIZE + n;
            if n == 0 || total.is_none_or(|t| seen as u64 >= t) {
                break;
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl ImportList for ReelixList {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        let mut out = Vec::new();
        if self.filter.allows(MediaKind::Movie) {
            out.extend(self.collect("/movies", MediaKind::Movie).await?);
        }
        if self.filter.allows(MediaKind::Series) {
            out.extend(self.collect("/series", MediaKind::Series).await?);
        }
        Ok(dedupe(out))
    }

    async fn test(&self) -> Result<(), ListError> {
        common::send(self.get("/system/status")).await.map(|_| ())
    }
}
