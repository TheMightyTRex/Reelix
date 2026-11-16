//! NTS (RFX-98): NTS 1 and 2's shows from the public, undocumented JSON at `nts.live/api/v2`.
//!
//! - Shows: `/api/v2/shows` (paged, about 1,700). NTS has no text search, so search reads the list once (cached
//!   for a day) and filters it here, by name and by genre.
//! - A show: `/api/v2/shows/{alias}` and its episodes `/api/v2/shows/{alias}/episodes`.
//! - Episode audio is on Mixcloud or SoundCloud; it arrives only through catch-up (yt-dlp on the episode's NTS page,
//!   [`crate::catchup`]). Off by default: NTS's terms are for listening on its own pages and apps.

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::provider::{
    DISCOVER_TTL, DiscoverList, EpisodeData, ProviderError, RadioProvider, SearchPrefs, SeriesData, ShowData,
    ShowFetch, ShowHit, async_trait,
};
use crate::providers::http::{JsonReader, str_at, url_at};
use crate::providers::rss::plain_text;

pub const ID: &str = "nts";
pub const BASE: &str = "https://www.nts.live";
pub const CACHE_TTL: Duration = Duration::from_secs(3600);
/// The whole show list is read at most once a day.
const LIST_TTL: Duration = Duration::from_secs(24 * 3600);
const PAGE: u32 = 50;
const MAX_LIST_PAGES: u32 = 50;
const MAX_EPISODES: u32 = 200;
const RESULTS: usize = 25;

pub struct NtsProvider {
    api: JsonReader,
    base: String,
}

impl NtsProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            api: JsonReader::new(http, "NTS", Duration::from_millis(500), CACHE_TTL),
            base: BASE.to_owned(),
        }
    }

    /// Another host (tests).
    pub fn with_base_url(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_owned();
        self
    }

    /// Spacing between requests (tests use zero).
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.api = self.api.with_interval(interval);
        self
    }

    /// Every show, a page at a time, cached for a day.
    async fn all_shows(&self) -> Result<Vec<Value>, ProviderError> {
        let mut out = Vec::new();
        for page in 0..MAX_LIST_PAGES {
            let url = format!("{}/api/v2/shows?offset={}&limit={PAGE}", self.base, page * PAGE);
            let data = self.api.get_for(&url, HeaderMap::new(), LIST_TTL).await?;
            let results = data
                .get("results")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let done = (results.len() as u32) < PAGE;
            out.extend(results);
            if done {
                break;
            }
        }
        Ok(out)
    }
}

fn is_alias(s: &str) -> bool {
    (1..=120).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn genres(v: &Value) -> Vec<String> {
    v.get("genres")
        .and_then(Value::as_array)
        .map(|g| {
            g.iter()
                .filter_map(|x| str_at(x, "value"))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn picture(v: &Value) -> Option<String> {
    v.get("media")
        .and_then(|m| url_at(m, "picture_large").or_else(|| url_at(m, "background_large")))
}

fn hit(v: &Value) -> Option<ShowHit> {
    let alias = str_at(v, "show_alias").filter(|a| is_alias(a))?;
    Some(ShowHit {
        provider: ID.to_owned(),
        external_id: alias.to_owned(),
        title: str_at(v, "name")?.trim().to_owned(),
        synopsis: str_at(v, "description").map(plain_text).filter(|s| !s.is_empty()),
        author: Some("NTS".to_owned()),
        station_name: Some("NTS".to_owned()),
        artwork_url: picture(v),
        web_url: Some(format!("{BASE}/shows/{alias}")),
        feed_url: None,
        episode_count: None,
        show_id: None,
        broadcaster: Some("NTS".to_owned()),
    })
}

/// Shows whose name, or one of whose genres, holds every word of `query` (case ignored).
pub fn matching(shows: &[Value], query: &str) -> Vec<ShowHit> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut named = Vec::new();
    let mut by_genre = Vec::new();
    for s in shows {
        let name = str_at(s, "name").unwrap_or_default().to_lowercase();
        let tags = genres(s).join(" ").to_lowercase();
        if words.iter().all(|w| name.contains(w.as_str())) {
            named.extend(hit(s));
        } else if words
            .iter()
            .all(|w| tags.contains(w.as_str()) || name.contains(w.as_str()))
        {
            by_genre.extend(hit(s));
        }
    }
    named.extend(by_genre);
    named.truncate(RESULTS);
    named
}

fn show_data(show: &Value, episodes: &[Value]) -> Result<ShowData, ProviderError> {
    let alias = str_at(show, "show_alias")
        .filter(|a| is_alias(a))
        .ok_or_else(|| ProviderError::NotFound("NTS has no such show".into()))?;
    let series = vec![SeriesData {
        external_id: "all".to_owned(),
        ..SeriesData::default()
    }];
    let eps: Vec<EpisodeData> = episodes
        .iter()
        .filter_map(|e| {
            let ep = str_at(e, "episode_alias").filter(|a| is_alias(a))?;
            let tags = genres(e);
            let mut synopsis = str_at(e, "description").map(plain_text).unwrap_or_default();
            if !tags.is_empty() {
                synopsis = format!(
                    "{}{}Genres: {}.",
                    synopsis,
                    if synopsis.is_empty() { "" } else { "\n\n" },
                    tags.join(", ")
                );
            }
            Some(EpisodeData {
                // `{show}/{episode}`, so catch-up finds the episode's page from the id alone.
                external_id: format!("{alias}/{ep}"),
                series_external_id: "all".to_owned(),
                number: None,
                title: str_at(e, "name").unwrap_or("Untitled episode").trim().to_owned(),
                synopsis: Some(synopsis).filter(|s| !s.is_empty()),
                first_broadcast: str_at(e, "broadcast")
                    .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                    .map(|d| d.with_timezone(&Utc)),
                duration_secs: None,
                enclosure_url: None,
                guid: None,
            })
        })
        .collect();
    Ok(ShowData {
        external_id: alias.to_owned(),
        title: str_at(show, "name").unwrap_or(alias).trim().to_owned(),
        synopsis: str_at(show, "description")
            .map(plain_text)
            .filter(|s| !s.is_empty()),
        author: Some("NTS".to_owned()),
        web_url: Some(format!("{BASE}/shows/{alias}")),
        artwork_url: picture(show),
        first_broadcast: eps.iter().filter_map(|e| e.first_broadcast).min(),
        series,
        episodes: eps,
        complete: true,
        ..ShowData::default()
    })
}

/// The show alias in an NTS link (`nts.live/shows/{alias}` or one of its episodes).
pub fn parse_link(link: &str) -> Option<String> {
    let url = url::Url::parse(link.trim()).ok()?;
    if !matches!(url.host_str()?, "nts.live" | "www.nts.live") {
        return None;
    }
    let mut parts = url.path_segments()?;
    (parts.next()? == "shows").then_some(())?;
    parts.next().filter(|a| is_alias(a)).map(str::to_owned)
}

#[async_trait]
impl RadioProvider for NtsProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "NTS"
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn default_on(&self) -> bool {
        false
    }

    fn episodes_arrive(&self) -> &'static str {
        "Catch-up: each episode's audio is fetched from its NTS page (Mixcloud or SoundCloud) when catch-up is on"
    }

    fn risk_note(&self) -> Option<&'static str> {
        Some(
            "NTS's terms are for listening on its own site and apps. Downloads are for your personal use only.",
        )
    }

    fn has_catchup(&self) -> bool {
        true
    }

    fn catchup_url(&self, episode_external_id: &str) -> Option<String> {
        let (show, ep) = episode_external_id.split_once('/')?;
        (is_alias(show) && is_alias(ep)).then(|| format!("{BASE}/shows/{show}/episodes/{ep}"))
    }

    async fn search(&self, query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        Ok(matching(&self.all_shows().await?, query))
    }

    fn discover_lists(&self) -> Vec<DiscoverList> {
        vec![DiscoverList::row("latest", "New on NTS")]
    }

    async fn discover(&self, list: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        if list != "latest" {
            return Err(ProviderError::NotFound(format!(
                "NTS has no Discover list {list}"
            )));
        }
        let url = format!("{}/api/v2/shows?offset=0&limit={PAGE}", self.base);
        let data = self.api.get_for(&url, HeaderMap::new(), DISCOVER_TTL).await?;
        Ok(data
            .get("results")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(hit).collect())
            .unwrap_or_default())
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        let Some(alias) = parse_link(link) else {
            return Ok(None);
        };
        self.show(
            &alias,
            &ShowFetch {
                quick: true,
                ..ShowFetch::default()
            },
        )
        .await
        .map(Some)
    }

    async fn show(&self, external_id: &str, fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let alias = external_id.trim();
        if !is_alias(alias) {
            return Err(ProviderError::NotFound(format!("{alias} is not an NTS show")));
        }
        let show = self
            .api
            .get(&format!("{}/api/v2/shows/{alias}", self.base), HeaderMap::new())
            .await?;
        let limit = if fetch.quick { 12 } else { MAX_EPISODES };
        let mut episodes = Vec::new();
        let mut offset = 0;
        while (episodes.len() as u32) < limit {
            let url = format!(
                "{}/api/v2/shows/{alias}/episodes?offset={offset}&limit={PAGE}",
                self.base
            );
            let data = self.api.get(&url, HeaderMap::new()).await?;
            let page = data
                .get("results")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = page.len() as u32;
            episodes.extend(page);
            if n < PAGE {
                break;
            }
            offset += PAGE;
        }
        episodes.truncate(limit as usize);
        show_data(&show, &episodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_catchup_pages() {
        assert_eq!(
            parse_link("https://www.nts.live/shows/100-elements").as_deref(),
            Some("100-elements")
        );
        assert_eq!(
            parse_link("https://www.nts.live/shows/100-elements/episodes/100-elements-9th-october-2026")
                .as_deref(),
            Some("100-elements")
        );
        assert_eq!(parse_link("https://www.mixcloud.com/x/"), None);
        let p = NtsProvider::new(reqwest::Client::new());
        assert_eq!(
            p.catchup_url("100-elements/100-elements-9th-october-2026")
                .as_deref(),
            Some("https://www.nts.live/shows/100-elements/episodes/100-elements-9th-october-2026")
        );
        assert_eq!(p.catchup_url("../etc/passwd"), None);
        assert!(!p.default_on());
    }

    #[test]
    fn search_by_name_then_genre() {
        let shows: Vec<Value> = serde_json::from_str(
            r#"[{"show_alias":"a","name":"Beats Hour","genres":[]},
                {"show_alias":"b","name":"Morning","genres":[{"value":"Beats"}]},
                {"show_alias":"c","name":"Jazz Club","genres":[{"value":"Jazz"}]}]"#,
        )
        .unwrap();
        let ids: Vec<String> = matching(&shows, "beats")
            .into_iter()
            .map(|h| h.external_id)
            .collect();
        assert_eq!(ids, ["a", "b"]);
        assert!(matching(&shows, "  ").is_empty());
    }
}
