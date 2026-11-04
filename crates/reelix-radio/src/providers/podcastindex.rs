//! Podcast Index (<https://podcastindex.org>): an open directory of podcast feeds.
//!
//! Needs a free API key and secret (radio settings `podcastindex_api_key` and
//! `podcastindex_api_secret`); until both are set the source counts as turned off. Every
//! request is signed: `X-Auth-Key`, `X-Auth-Date` (unix seconds) and
//! `Authorization: sha1(key + secret + date)` as lower-case hex.
//!
//! Search: `/search/byterm?q=` (hits for the settings' language first). Shows are saved as
//! `podcastindex` shows with the feed id as external id: `/podcasts/byfeedid?id=` for the show
//! and `/episodes/byfeedid?id=&max=` for its episodes, which map like a feed's items (season →
//! series, trailers and bonus episodes → specials). A `podcastindex.org/podcast/{id}` link
//! previews.

use std::sync::RwLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reelix_api_types::radio::{RadioSeriesKind, RadioSettingsDto};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::provider::{
    EpisodeData, ProviderError, RadioProvider, SearchPrefs, SeriesData, ShowData, ShowFetch, ShowHit,
    async_trait,
};
use crate::providers::http::{JsonReader, str_at, u32_at, url_at};
use crate::providers::rss::plain_text;

pub const ID: &str = "podcastindex";

pub const BASE: &str = "https://api.podcastindex.org/api/1.0";
pub const CACHE_TTL: Duration = Duration::from_secs(300);
pub const REQUEST_INTERVAL: Duration = Duration::from_millis(250);
const SEARCH_LIMIT: u32 = 25;
/// Most episodes read per show (the API's maximum).
const MAX_EPISODES: u32 = 1000;

const NOT_CONFIGURED: &str =
    "Podcast Index needs an API key and secret (free at api.podcastindex.org) in the radio settings";

#[derive(Clone)]
struct Credentials {
    key: String,
    secret: String,
}

pub struct PodcastIndexProvider {
    api: JsonReader,
    base: String,
    credentials: RwLock<Option<Credentials>>,
}

impl PodcastIndexProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            api: JsonReader::new(http, "Podcast Index", REQUEST_INTERVAL, CACHE_TTL),
            base: BASE.to_owned(),
            credentials: RwLock::new(None),
        }
    }

    /// Another API host (tests).
    pub fn with_base_url(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_owned();
        self
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.api = self.api.with_interval(interval);
        self
    }

    fn credentials(&self) -> Result<Credentials, ProviderError> {
        self.credentials
            .read()
            .ok()
            .and_then(|c| c.clone())
            .ok_or_else(|| ProviderError::Unsupported(NOT_CONFIGURED.into()))
    }

    async fn get(&self, path_and_query: &str) -> Result<std::sync::Arc<Value>, ProviderError> {
        let c = self.credentials()?;
        let date = Utc::now().timestamp().to_string();
        let mut h = HeaderMap::new();
        let value = |s: &str| {
            HeaderValue::from_str(s).map_err(|_| {
                ProviderError::Unsupported(
                    "the Podcast Index key or secret has characters that cannot be sent".into(),
                )
            })
        };
        h.insert("X-Auth-Key", value(&c.key)?);
        h.insert("X-Auth-Date", value(&date)?);
        h.insert(
            reqwest::header::AUTHORIZATION,
            value(&signature(&c.key, &c.secret, &date))?,
        );
        self.api.get(&format!("{}{path_and_query}", self.base), h).await
    }
}

/// `sha1(key + secret + date)` as lower-case hex.
pub fn signature(key: &str, secret: &str, date: &str) -> String {
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(secret.as_bytes());
    h.update(date.as_bytes());
    hex::encode(h.finalize())
}

/// The feed id in a `podcastindex.org/podcast/{id}` link.
pub fn parse_link(link: &str) -> Option<String> {
    let u = url::Url::parse(link.trim()).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if !matches!(u.scheme(), "http" | "https")
        || !matches!(host.as_str(), "podcastindex.org" | "www.podcastindex.org")
    {
        return None;
    }
    let seg: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    match seg.as_slice() {
        ["podcast", id, ..] if is_id(id) => Some((*id).to_owned()),
        _ => None,
    }
}

fn is_id(s: &str) -> bool {
    (1..=15).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

fn id_of(v: &Value) -> Option<String> {
    let x = v.get("id")?;
    x.as_u64()
        .map(|n| n.to_string())
        .or_else(|| x.as_str().filter(|s| is_id(s)).map(str::to_owned))
}

fn artwork(v: &Value) -> Option<String> {
    url_at(v, "artwork").or_else(|| url_at(v, "image"))
}

fn hit(f: &Value) -> Option<ShowHit> {
    Some(ShowHit {
        provider: ID.to_owned(),
        external_id: id_of(f)?,
        title: str_at(f, "title")?.to_owned(),
        synopsis: str_at(f, "description").map(plain_text).filter(|s| !s.is_empty()),
        author: str_at(f, "author")
            .or_else(|| str_at(f, "ownerName"))
            .map(str::to_owned),
        station_name: None,
        artwork_url: artwork(f),
        web_url: url_at(f, "link"),
        feed_url: url_at(f, "url"),
        episode_count: u32_at(f, "episodeCount"),
        show_id: None,
    })
}

/// Show + episodes from `/podcasts/byfeedid` and `/episodes/byfeedid`.
fn show_data(feed: &Value, items: &[Value]) -> Result<ShowData, ProviderError> {
    let id = id_of(feed).ok_or_else(|| ProviderError::NotFound("Podcast Index has no such feed".into()))?;
    let title = str_at(feed, "title").unwrap_or("Untitled podcast").to_owned();
    let mut series: Vec<SeriesData> = Vec::new();
    let mut episodes = Vec::new();
    for it in items {
        let Some(external_id) = str_at(it, "guid").map(str::to_owned).or_else(|| id_of(it)) else {
            continue;
        };
        let special = matches!(
            str_at(it, "episodeType").map(str::to_ascii_lowercase).as_deref(),
            Some("trailer" | "bonus")
        );
        let season = u32_at(it, "season");
        let (sid, kind, number) = match (special, season) {
            (true, _) => ("specials".to_owned(), RadioSeriesKind::Specials, None),
            (false, Some(n)) => (format!("season:{n}"), RadioSeriesKind::Series, Some(n)),
            (false, None) => ("season:1".to_owned(), RadioSeriesKind::Series, None),
        };
        match series.iter_mut().find(|s| s.external_id == sid) {
            Some(s) if s.number.is_none() => s.number = number,
            Some(_) => {}
            None => series.push(SeriesData {
                external_id: sid.clone(),
                kind,
                number,
                ..SeriesData::default()
            }),
        }
        episodes.push(EpisodeData {
            external_id,
            series_external_id: sid,
            number: u32_at(it, "episode"),
            title: str_at(it, "title").unwrap_or("Untitled episode").to_owned(),
            synopsis: str_at(it, "description")
                .map(plain_text)
                .filter(|s| !s.is_empty()),
            first_broadcast: it
                .get("datePublished")
                .and_then(Value::as_i64)
                .and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)),
            duration_secs: u32_at(it, "duration"),
            enclosure_url: url_at(it, "enclosureUrl"),
            guid: str_at(it, "guid").map(str::to_owned),
        });
    }
    let first_broadcast = episodes.iter().filter_map(|e| e.first_broadcast).min();
    Ok(ShowData {
        external_id: id,
        title,
        synopsis: str_at(feed, "description")
            .map(plain_text)
            .filter(|s| !s.is_empty()),
        author: str_at(feed, "author")
            .or_else(|| str_at(feed, "ownerName"))
            .map(str::to_owned),
        web_url: url_at(feed, "link"),
        feed_url: url_at(feed, "url"),
        artwork_url: artwork(feed),
        first_broadcast,
        series,
        episodes,
        complete: true,
        ..ShowData::default()
    })
}

#[async_trait]
impl RadioProvider for PodcastIndexProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Podcast Index"
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn configure(&self, settings: &RadioSettingsDto) {
        let c = match (
            settings.podcastindex_api_key.as_deref().map(str::trim),
            settings.podcastindex_api_secret.as_deref().map(str::trim),
        ) {
            (Some(k), Some(s)) if !k.is_empty() && !s.is_empty() => Some(Credentials {
                key: k.to_owned(),
                secret: s.to_owned(),
            }),
            _ => None,
        };
        if let Ok(mut cur) = self.credentials.write() {
            *cur = c;
        }
    }

    fn is_configured(&self) -> bool {
        self.credentials.read().is_ok_and(|c| c.is_some())
    }

    async fn search(&self, query: &str, prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let q: String = url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect();
        let data = self
            .get(&format!("/search/byterm?q={q}&max={SEARCH_LIMIT}"))
            .await?;
        let feeds = data
            .get("feeds")
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::Parse("Podcast Index sent an unexpected answer".into()))?;
        let mut feeds: Vec<&Value> = feeds.iter().collect();
        if let Some(lang) = &prefs.language {
            // Stable: the settings' language first.
            feeds.sort_by_key(|f| {
                !str_at(f, "language").is_some_and(|l| l.to_ascii_lowercase().starts_with(lang.as_str()))
            });
        }
        Ok(feeds.into_iter().filter_map(hit).collect())
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        let Some(id) = parse_link(link) else {
            return Ok(None);
        };
        self.show(&id, &ShowFetch::default()).await.map(Some)
    }

    async fn show(&self, external_id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let id = external_id.trim();
        if !is_id(id) {
            return Err(ProviderError::NotFound(format!(
                "{id} is not a Podcast Index feed id"
            )));
        }
        let podcast = self.get(&format!("/podcasts/byfeedid?id={id}")).await?;
        // An unknown id answers `"feed": []`.
        let feed = podcast
            .get("feed")
            .filter(|f| f.is_object())
            .ok_or_else(|| ProviderError::NotFound(format!("Podcast Index has no feed {id}")))?;
        let eps = self
            .get(&format!("/episodes/byfeedid?id={id}&max={MAX_EPISODES}"))
            .await?;
        let items = eps
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::Parse("Podcast Index sent an unexpected answer".into()))?;
        show_data(feed, items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_like_the_docs() {
        // sha1("key" + "secret" + "1700000000")
        assert_eq!(
            signature("key", "secret", "1700000000"),
            "abaf71c02050c31e4d4e6b08c1625173af0445ba"
        );
    }

    #[test]
    fn links() {
        assert_eq!(
            parse_link("https://podcastindex.org/podcast/920666").as_deref(),
            Some("920666")
        );
        assert_eq!(parse_link("https://podcastindex.org/search?q=x"), None);
        assert_eq!(parse_link("https://example.com/podcast/920666"), None);
    }
}
