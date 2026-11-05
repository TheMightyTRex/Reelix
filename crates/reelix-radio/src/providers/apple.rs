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
//!
//! Discover lists (cached 30 minutes, for the settings' country, `US` when none is set):
//! - `top`: Apple's top 50 shows from the Marketing Tools RSS
//!   (`rss.marketingtools.apple.com/api/v2/{cc}/podcasts/top/50/podcasts.json`);
//! - `genre-{id}`: the top 50 of one Apple Podcasts category from the older iTunes RSS
//!   (`itunes.apple.com/{cc}/rss/toppodcasts/limit=50/genre={id}/json`; Marketing Tools has no
//!   categories).
//!
//! Chart hits carry `provider: "apple"` and the Apple id; the feed is looked up only when a
//! show is opened or added ([`RadioProvider::show`]), not for the whole chart.

use std::time::Duration;

use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::provider::{
    DISCOVER_TTL, DiscoverList, ProviderError, RadioProvider, SearchPrefs, ShowData, ShowFetch, ShowHit,
    async_trait,
};
use crate::providers::http::{JsonReader, str_at, u32_at, url_at};
use crate::providers::rss::{self, RssProvider};

pub const ID: &str = "apple";

pub const BASE: &str = "https://itunes.apple.com";
/// Apple Marketing Tools RSS (charts).
pub const CHARTS_BASE: &str = "https://rss.marketingtools.apple.com";
/// Searches are cached for five minutes, lookups for an hour (one reader; the TTL is the
/// shorter).
pub const CACHE_TTL: Duration = Duration::from_secs(300);
/// Apple allows about 20 requests a minute.
pub const REQUEST_INTERVAL: Duration = Duration::from_millis(500);
const SEARCH_LIMIT: u32 = 25;
/// Shows per chart.
const CHART_LIMIT: usize = 50;

/// Apple Podcasts top-level categories offered as Discover lists: `(genre id, name)`.
pub const GENRES: &[(u32, &str)] = &[
    (1301, "Arts"),
    (1321, "Business"),
    (1303, "Comedy"),
    (1304, "Education"),
    (1483, "Fiction"),
    (1511, "Government"),
    (1512, "Health & Fitness"),
    (1487, "History"),
    (1305, "Kids & Family"),
    (1502, "Leisure"),
    (1310, "Music"),
    (1489, "News"),
    (1314, "Religion & Spirituality"),
    (1533, "Science"),
    (1324, "Society & Culture"),
    (1545, "Sports"),
    (1318, "Technology"),
    (1488, "True Crime"),
    (1309, "TV & Film"),
];

pub struct AppleProvider {
    api: JsonReader,
    feeds: RssProvider,
    base: String,
    charts_base: String,
}

impl AppleProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            api: JsonReader::new(http.clone(), "Apple Podcasts", REQUEST_INTERVAL, CACHE_TTL),
            feeds: RssProvider::new(http),
            base: BASE.to_owned(),
            charts_base: CHARTS_BASE.to_owned(),
        }
    }

    /// Another API host (tests).
    pub fn with_base_url(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_owned();
        self
    }

    /// Another Marketing Tools host (tests).
    pub fn with_charts_url(mut self, base: &str) -> Self {
        self.charts_base = base.trim_end_matches('/').to_owned();
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

/// Apple artwork in 600×600: chart images name their size in the last path segment
/// (`…/100x100bb.png`).
fn large_artwork(url: &str) -> String {
    match url.rsplit_once('/') {
        Some((head, last))
            if last.split_once("bb.").is_some_and(|(size, _)| {
                size.split('x')
                    .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            }) =>
        {
            format!("{head}/600x600bb.jpg")
        }
        _ => url.to_owned(),
    }
}

fn chart_hit(
    id: String,
    title: &str,
    author: Option<&str>,
    artwork: Option<String>,
    web: Option<String>,
) -> ShowHit {
    ShowHit {
        provider: ID.to_owned(),
        external_id: id,
        title: title.to_owned(),
        synopsis: None,
        author: author.map(str::to_owned),
        station_name: None,
        artwork_url: artwork.map(|a| large_artwork(&a)),
        web_url: web.map(|u| u.split('?').next().unwrap_or(&u).to_owned()),
        feed_url: None,
        episode_count: None,
        show_id: None,
    }
}

/// Shows of a Marketing Tools chart (`feed.results`).
pub fn parse_top_chart(data: &Value) -> Result<Vec<ShowHit>, ProviderError> {
    let results = data
        .get("feed")
        .and_then(|f| f.get("results"))
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Parse("the Apple Podcasts chart has changed".into()))?;
    Ok(results
        .iter()
        .filter_map(|r| {
            let id = str_at(r, "id").filter(|i| is_id(i))?.to_owned();
            Some(chart_hit(
                id,
                str_at(r, "name")?,
                str_at(r, "artistName"),
                url_at(r, "artworkUrl100"),
                url_at(r, "url"),
            ))
        })
        .take(CHART_LIMIT)
        .collect())
}

/// `v.key.label` of the iTunes RSS.
fn label<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?
        .get("label")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Shows of an iTunes RSS chart (`feed.entry`, an object when there is only one).
pub fn parse_genre_chart(data: &Value) -> Result<Vec<ShowHit>, ProviderError> {
    let feed = data
        .get("feed")
        .filter(|f| f.is_object())
        .ok_or_else(|| ProviderError::Parse("the Apple Podcasts chart has changed".into()))?;
    let entries: Vec<&Value> = match feed.get("entry") {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(e @ Value::Object(_)) => vec![e],
        // An empty chart has no `entry`.
        _ => Vec::new(),
    };
    Ok(entries
        .into_iter()
        .filter_map(|e| {
            let id = e
                .get("id")
                .and_then(|i| i.get("attributes"))
                .and_then(|a| str_at(a, "im:id"))
                .filter(|i| is_id(i))?
                .to_owned();
            let artwork = e
                .get("im:image")
                .and_then(Value::as_array)
                .and_then(|imgs| imgs.last())
                .and_then(|i| i.get("label"))
                .and_then(Value::as_str)
                .filter(|u| u.starts_with("https://"))
                .map(str::to_owned);
            let web = e
                .get("link")
                .and_then(|l| l.get("attributes"))
                .and_then(|a| url_at(a, "href"));
            let mut hit = chart_hit(id, label(e, "im:name")?, label(e, "im:artist"), artwork, web);
            hit.synopsis = label(e, "summary").map(str::to_owned);
            Some(hit)
        })
        .take(CHART_LIMIT)
        .collect())
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

    fn discover_lists(&self) -> Vec<DiscoverList> {
        std::iter::once(DiscoverList::row("top", "Top podcasts"))
            .chain(
                GENRES
                    .iter()
                    .map(|(id, name)| DiscoverList::category(&format!("genre-{id}"), name)),
            )
            .collect()
    }

    async fn discover(&self, list: &str, prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let cc = prefs.country.as_deref().unwrap_or("US").to_ascii_lowercase();
        if !(cc.len() == 2 && cc.bytes().all(|b| b.is_ascii_lowercase())) {
            return Err(ProviderError::Unsupported(format!("{cc} is not a country code")));
        }
        if list == "top" {
            let url = format!(
                "{}/api/v2/{cc}/podcasts/top/{CHART_LIMIT}/podcasts.json",
                self.charts_base
            );
            let data = self.api.get_for(&url, HeaderMap::new(), DISCOVER_TTL).await?;
            return parse_top_chart(&data);
        }
        let genre = list
            .strip_prefix("genre-")
            .and_then(|g| g.parse::<u32>().ok())
            .filter(|g| GENRES.iter().any(|(id, _)| id == g))
            .ok_or_else(|| ProviderError::NotFound(format!("Apple Podcasts has no Discover list {list}")))?;
        let url = format!(
            "{}/{cc}/rss/toppodcasts/limit={CHART_LIMIT}/genre={genre}/json",
            self.base
        );
        let data = self.api.get_for(&url, HeaderMap::new(), DISCOVER_TTL).await?;
        parse_genre_chart(&data)
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

    #[test]
    fn artwork_sizes() {
        assert_eq!(
            large_artwork("https://is1-ssl.mzstatic.com/image/thumb/x/mza_1.jpg/100x100bb.png"),
            "https://is1-ssl.mzstatic.com/image/thumb/x/mza_1.jpg/600x600bb.jpg"
        );
        assert_eq!(
            large_artwork("https://is1-ssl.mzstatic.com/a/b.jpg"),
            "https://is1-ssl.mzstatic.com/a/b.jpg"
        );
    }
}
