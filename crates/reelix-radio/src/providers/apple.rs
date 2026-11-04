//! Apple Podcasts: a directory of podcast feeds (the iTunes Search API).
//!
//! Search uses `https://itunes.apple.com/search?media=podcast&entity=podcast&term=…&country=…`
//! (the settings' country, `US` when none is set). A `podcasts.apple.com/…/id{n}` link is
//! looked up (`/lookup?id=`) for its feed URL.
//!
//! Apple only knows where the feed is, so shows are **handed to the feed provider**: hits
//! carry `provider: "rss"` and the feed URL as `external_id`, and a previewed link returns
//! the feed's data with [`ShowData::provider`] = `rss`. The show is then saved, refreshed
//! (hourly) and de-duplicated as a feed, whichever directory found it, and keeps working if
//! Apple's API changes. Feeds are read with [`RssProvider`].

use std::time::Duration;

use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::provider::{ProviderError, RadioProvider, SearchPrefs, ShowData, ShowFetch, ShowHit, async_trait};
use crate::providers::http::{JsonReader, str_at, u32_at, url_at};
use crate::providers::rss::{self, RssProvider};

pub const ID: &str = "apple";

pub const BASE: &str = "https://itunes.apple.com";
/// Searches are cached for five minutes, lookups for an hour (one reader; the TTL is the
/// shorter).
pub const CACHE_TTL: Duration = Duration::from_secs(300);
/// Apple allows about 20 requests a minute.
pub const REQUEST_INTERVAL: Duration = Duration::from_millis(500);
const SEARCH_LIMIT: u32 = 25;

pub struct AppleProvider {
    api: JsonReader,
    feeds: RssProvider,
    base: String,
}

impl AppleProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            api: JsonReader::new(http.clone(), "Apple Podcasts", REQUEST_INTERVAL, CACHE_TTL),
            feeds: RssProvider::new(http),
            base: BASE.to_owned(),
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

    /// The feed URL of the podcast with Apple id `id`.
    async fn feed_of(&self, id: &str) -> Result<String, ProviderError> {
        let data = self
            .api
            .get(
                &format!("{}/lookup?id={id}&entity=podcast", self.base),
                HeaderMap::new(),
            )
            .await?;
        let rs = results(&data)?;
        rs.iter()
            .find(|r| {
                r.get("collectionId")
                    .and_then(Value::as_u64)
                    .map(|n| n.to_string())
                    .as_deref()
                    == Some(id)
            })
            .or_else(|| rs.first())
            .and_then(|r| url_at(r, "feedUrl"))
            .ok_or_else(|| ProviderError::NotFound(format!("Apple Podcasts has no feed for podcast {id}")))
    }

    /// The feed's show, handed to `rss`.
    async fn read_feed(&self, feed: &str) -> Result<ShowData, ProviderError> {
        let mut data = self
            .feeds
            .read(feed)
            .await?
            .ok_or_else(|| ProviderError::Parse(format!("{feed} is not a podcast feed")))?;
        data.provider = Some(rss::ID.to_owned());
        Ok(data)
    }
}

fn results(data: &Value) -> Result<&Vec<Value>, ProviderError> {
    data.get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Parse("Apple Podcasts sent an unexpected answer".into()))
}

/// The Apple podcast id in a `podcasts.apple.com` (or `itunes.apple.com`) link.
pub fn parse_link(link: &str) -> Option<String> {
    let u = url::Url::parse(link.trim()).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if !matches!(u.scheme(), "http" | "https")
        || !matches!(host.as_str(), "podcasts.apple.com" | "itunes.apple.com")
    {
        return None;
    }
    let from_path = u
        .path_segments()?
        .filter_map(|s| s.strip_prefix("id"))
        .find(|d| is_id(d))
        .map(str::to_owned);
    from_path.or_else(|| {
        u.query_pairs()
            .find(|(k, v)| k == "id" && is_id(v))
            .map(|(_, v)| v.into_owned())
    })
}

fn is_id(s: &str) -> bool {
    (1..=15).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

fn hit(r: &Value) -> Option<ShowHit> {
    let feed = url_at(r, "feedUrl")?;
    Some(ShowHit {
        provider: rss::ID.to_owned(),
        external_id: feed.clone(),
        title: str_at(r, "collectionName")
            .or_else(|| str_at(r, "trackName"))?
            .to_owned(),
        synopsis: None,
        author: str_at(r, "artistName").map(str::to_owned),
        station_name: None,
        artwork_url: url_at(r, "artworkUrl600").or_else(|| url_at(r, "artworkUrl100")),
        web_url: url_at(r, "collectionViewUrl").map(|u| u.split('?').next().unwrap_or(&u).to_owned()),
        feed_url: Some(feed),
        episode_count: u32_at(r, "trackCount"),
        show_id: None,
    })
}

#[async_trait]
impl RadioProvider for AppleProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Apple Podcasts"
    }

    fn supports_search(&self) -> bool {
        true
    }

    async fn search(&self, query: &str, prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let country = prefs.country.as_deref().unwrap_or("US");
        let limit = SEARCH_LIMIT.to_string();
        let url = url::Url::parse_with_params(
            &format!("{}/search", self.base),
            &[
                ("media", "podcast"),
                ("entity", "podcast"),
                ("term", query.trim()),
                ("country", country),
                ("limit", limit.as_str()),
            ],
        )
        .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let data = self.api.get(url.as_str(), HeaderMap::new()).await?;
        Ok(results(&data)?.iter().filter_map(hit).collect())
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        let Some(id) = parse_link(link) else {
            return Ok(None);
        };
        let feed = self.feed_of(&id).await?;
        self.read_feed(&feed).await.map(Some)
    }

    /// `external_id` is an Apple id (or a feed URL); the show comes back handed to `rss`.
    async fn show(&self, external_id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let id = external_id.trim();
        let feed = if is_id(id) {
            self.feed_of(id).await?
        } else if id.starts_with("https://") || id.starts_with("http://") {
            id.to_owned()
        } else {
            return Err(ProviderError::NotFound(format!(
                "{id} is not an Apple Podcasts id"
            )));
        };
        self.read_feed(&feed).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        assert_eq!(
            parse_link("https://podcasts.apple.com/gb/podcast/just-a-minute/id1767854856?uo=4").as_deref(),
            Some("1767854856")
        );
        assert_eq!(
            parse_link("https://podcasts.apple.com/podcast/id1767854856").as_deref(),
            Some("1767854856")
        );
        assert_eq!(
            parse_link("https://itunes.apple.com/lookup?id=1767854856").as_deref(),
            Some("1767854856")
        );
        assert_eq!(parse_link("https://podcasts.apple.com/gb/podcast/x/idabc"), None);
        assert_eq!(parse_link("https://example.com/id1767854856"), None);
    }
}
