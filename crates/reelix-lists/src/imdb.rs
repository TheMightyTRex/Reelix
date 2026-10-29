//! IMDb public lists and watchlists via the CSV export, plus best-effort chart pages
//! (Top 250, Top TV, MovieMeter, TVMeter) scraped from embedded JSON-LD or title links.

use std::collections::HashSet;
use std::sync::LazyLock;

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use regex::Regex;
use serde_json::Value;

use crate::common::{self, KindFilter, Settings};
use crate::{BuildContext, ImportList, ListError, ListItem, dedupe};

pub const IMPLEMENTATION: &str = "imdb";

static LIST_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(ls|ur)\d{5,}\b").expect("regex"));
static TITLE_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/title/(tt\d{7,})").expect("regex"));
static JSON_LD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<script[^>]*type="application/ld\+json"[^>]*>(.*?)</script>"#).expect("regex")
});

/// What an IMDb list instance pulls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImdbListType {
    /// Public list `ls…` (CSV export).
    List,
    /// User watchlist `ur…` (CSV export; must be public).
    Watchlist,
    Top250,
    TopTv,
    MovieMeter,
    TvMeter,
}

impl ImdbListType {
    fn chart(self) -> Option<(&'static str, MediaKind)> {
        match self {
            Self::Top250 => Some(("/chart/top/", MediaKind::Movie)),
            Self::TopTv => Some(("/chart/toptv/", MediaKind::Series)),
            Self::MovieMeter => Some(("/chart/moviemeter/", MediaKind::Movie)),
            Self::TvMeter => Some(("/chart/tvmeter/", MediaKind::Series)),
            Self::List | Self::Watchlist => None,
        }
    }
}

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "IMDb".into(),
        description: Some(
            "Public IMDb lists and watchlists (CSV export) and chart pages (best effort).".into(),
        ),
        info_url: Some("https://www.imdb.com/".into()),
        fields: vec![
            FieldSchema::new(
                "list_type",
                "List type",
                common::select(&[
                    ("list", "List"),
                    ("watchlist", "Watchlist"),
                    ("top250", "Top 250 movies"),
                    ("top_tv", "Top 250 TV"),
                    ("moviemeter", "Most popular movies"),
                    ("tvmeter", "Most popular TV"),
                ]),
            )
            .required()
            .default_value("list"),
            FieldSchema::new("list_id", "List or user id", FieldKind::Text)
                .placeholder("ls012345678 or ur01234567")
                .help("A list id (ls…), user id (ur…) for watchlists, or the full URL."),
            FieldSchema::new("filter", "Include", common::select(KindFilter::OPTIONS)).default_value("both"),
            common::base_url_field("https://www.imdb.com"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured IMDb list.
#[derive(Debug, Clone)]
pub struct ImdbList {
    http: reqwest::Client,
    base_url: String,
    list_type: ImdbListType,
    list_id: Option<String>,
    filter: KindFilter,
}

impl ImdbList {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), settings)?;
        let list_type = match s.req("list_type")?.as_str() {
            "list" => ImdbListType::List,
            "watchlist" => ImdbListType::Watchlist,
            "top250" => ImdbListType::Top250,
            "top_tv" => ImdbListType::TopTv,
            "moviemeter" => ImdbListType::MovieMeter,
            "tvmeter" => ImdbListType::TvMeter,
            o => {
                return Err(ListError::InvalidSettings(format!(
                    "unknown IMDb list type {o:?}"
                )));
            }
        };
        let list_id = s
            .str("list_id")
            .and_then(|raw| LIST_ID.find(&raw).map(|m| m.as_str().to_string()));
        let expected = match list_type {
            ImdbListType::List => Some("ls"),
            ImdbListType::Watchlist => Some("ur"),
            _ => None,
        };
        if let Some(prefix) = expected
            && !list_id.as_deref().is_some_and(|id| id.starts_with(prefix))
        {
            return Err(ListError::InvalidSettings(format!(
                "list_id must be an IMDb {prefix}… id"
            )));
        }
        Ok(Self {
            http: ctx.http.clone(),
            base_url: s.base_url(&ctx.base_urls.imdb),
            list_type,
            list_id,
            filter: KindFilter::parse(s.str("filter"))?,
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{path}", self.base_url))
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.8")
            .header(
                reqwest::header::USER_AGENT,
                concat!("Mozilla/5.0 (compatible; Reelix/", env!("CARGO_PKG_VERSION"), ")"),
            )
    }
}

#[async_trait]
impl ImportList for ImdbList {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        let items = if let Some((path, kind)) = self.list_type.chart() {
            let html = common::send(self.get(path)).await?.text().await?;
            parse_chart(&html, kind)
        } else {
            let id = self.list_id.as_deref().unwrap_or_default();
            let path = match self.list_type {
                ImdbListType::Watchlist => format!("/user/{id}/watchlist/export"),
                _ => format!("/list/{id}/export"),
            };
            let text = common::send(self.get(&path)).await?.text().await?;
            parse_export_csv(&text)?
        };
        Ok(dedupe(
            items.into_iter().filter(|i| self.filter.allows(i.kind)).collect(),
        ))
    }
}

/// Minimal RFC 4180 reader: quoted fields, doubled quotes, CRLF/LF, leading BOM.
pub(crate) fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, in_quotes) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', true) => in_quotes = false,
            ('"', false) if field.is_empty() => in_quotes = true,
            (',', false) => row.push(std::mem::take(&mut field)),
            ('\r', false) => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            (c, _) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].trim().is_empty()));
    rows
}

/// Maps an IMDb "Title Type" to a media kind; `None` for episodes, games, podcasts, etc.
fn title_type_kind(t: &str) -> Option<MediaKind> {
    let norm: String = t
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    match norm.as_str() {
        "tvseries" | "tvminiseries" => Some(MediaKind::Series),
        "" | "movie" | "feature" | "tvmovie" | "video" | "short" | "tvshort" | "tvspecial"
        | "documentary" => Some(MediaKind::Movie),
        _ => None,
    }
}

/// Parses an IMDb list/watchlist CSV export (header row required; column order is not assumed).
pub fn parse_export_csv(text: &str) -> Result<Vec<ListItem>, ListError> {
    let mut rows = parse_csv(text).into_iter();
    let header = rows
        .next()
        .ok_or_else(|| ListError::Parse("empty CSV export".into()))?;
    let col = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
    let id_col = col("Const").ok_or_else(|| ListError::Parse("CSV export has no 'Const' column".into()))?;
    let (title_col, type_col, year_col) = (col("Title"), col("Title Type"), col("Year"));
    let cell = |r: &[String], c: Option<usize>| c.and_then(|c| r.get(c)).map(|s| s.trim().to_string());
    Ok(rows
        .filter_map(|r| {
            let imdb = common::imdb_id(r.get(id_col).map(|s| s.trim().to_string()))?;
            let kind = title_type_kind(&cell(&r, type_col).unwrap_or_default())?;
            Some(ListItem {
                year: cell(&r, year_col).and_then(|y| y.parse().ok()),
                imdb_id: Some(imdb),
                ..ListItem::new(kind, cell(&r, title_col).unwrap_or_default())
            })
        })
        .collect())
}

/// Best-effort chart parsing: JSON-LD `ItemList` first, falling back to `/title/tt…` links.
pub fn parse_chart(html: &str, kind: MediaKind) -> Vec<ListItem> {
    let mut out = Vec::new();
    for cap in JSON_LD.captures_iter(html) {
        let Ok(v) = serde_json::from_str::<Value>(&cap[1]) else {
            continue;
        };
        let Some(elements) = v.get("itemListElement").and_then(Value::as_array) else {
            continue;
        };
        for el in elements {
            let item = el.get("item").unwrap_or(el);
            let url = item.get("url").and_then(Value::as_str).unwrap_or_default();
            let Some(id) = TITLE_LINK.captures(url).map(|c| c[1].to_string()) else {
                continue;
            };
            let kind = match item.get("@type").and_then(Value::as_str) {
                Some("TVSeries") => MediaKind::Series,
                Some("Movie") => MediaKind::Movie,
                _ => kind,
            };
            out.push(ListItem {
                year: common::value_year(item.get("datePublished")),
                imdb_id: Some(id),
                ..ListItem::new(kind, common::value_str(item.get("name")).unwrap_or_default())
            });
        }
    }
    if out.is_empty() {
        let mut seen = HashSet::new();
        for c in TITLE_LINK.captures_iter(html) {
            let id = c[1].to_string();
            if seen.insert(id.clone()) {
                out.push(ListItem {
                    imdb_id: Some(id),
                    ..ListItem::new(kind, "")
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_handles_quotes_and_crlf() {
        let rows = parse_csv("\u{feff}a,b,c\r\n1,\"x, \"\"y\"\"\",3\r\n\r\n");
        assert_eq!(rows, vec![vec!["a", "b", "c"], vec!["1", "x, \"y\"", "3"]]);
    }

    #[test]
    fn chart_falls_back_to_links() {
        let html = r#"<a href="/title/tt0111161/">x</a><a href="/title/tt0111161/?ref">y</a><a href="/title/tt0068646/">z</a>"#;
        let items = parse_chart(html, MediaKind::Movie);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].imdb_id.as_deref(), Some("tt0068646"));
    }
}
