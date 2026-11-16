//! Mixcloud (RFX-99): stations and DJs on Mixcloud (Worldwide FM, Soho Radio, community and student stations, the
//! NTS archive) from its documented, key-free API at `api.mixcloud.com`.
//!
//! - A show is a Mixcloud account (`/{user}/`); its uploads (cloudcasts) are the episodes.
//! - Search: `/search/?type=user`; Discover: one list per genre tag (`/discover/{tag}/popular/`).
//! - Audio: catch-up only (yt-dlp on the upload's page, [`crate::catchup`]). Off by default, with a warning:
//!   Mixcloud's terms are for streaming on its own site and apps.

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::provider::{
    DISCOVER_TTL, DiscoverList, EpisodeData, ProviderError, RadioProvider, SearchPrefs, SeriesData, ShowData,
    ShowFetch, ShowHit, async_trait,
};
use crate::providers::http::{JsonReader, str_at, u32_at, url_at};
use crate::providers::rss::plain_text;

pub const ID: &str = "mixcloud";
pub const BASE: &str = "https://api.mixcloud.com";
pub const SITE: &str = "https://www.mixcloud.com";
pub const CACHE_TTL: Duration = Duration::from_secs(1800);
const PAGE: u32 = 100;
const MAX_EPISODES: usize = 300;

/// Genre tags offered as Discover lists: `(tag, name)`.
pub const TAGS: &[(&str, &str)] = &[
    ("radio", "Radio"),
    ("talk", "Talk"),
    ("jazz", "Jazz"),
    ("soul", "Soul"),
    ("electronic", "Electronic"),
    ("hip-hop", "Hip hop"),
    ("ambient", "Ambient"),
    ("classical", "Classical"),
    ("world", "World"),
];

pub struct MixcloudProvider {
    api: JsonReader,
    base: String,
}

impl MixcloudProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            api: JsonReader::new(http, "Mixcloud", Duration::from_millis(500), CACHE_TTL),
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
}

fn is_slug(s: &str) -> bool {
    (1..=120).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `/{user}/` from an API key.
fn user_of(key: &str) -> Option<&str> {
    let user = key.trim_matches('/');
    is_slug(user).then_some(user)
}

fn picture(v: &Value) -> Option<String> {
    v.get("pictures")
        .and_then(|p| url_at(p, "extra_large").or_else(|| url_at(p, "large")))
}

fn user_hit(u: &Value) -> Option<ShowHit> {
    let user = user_of(str_at(u, "key")?)?;
    Some(ShowHit {
        provider: ID.to_owned(),
        external_id: user.to_owned(),
        title: str_at(u, "name")?.trim().to_owned(),
        synopsis: str_at(u, "biog").map(plain_text).filter(|s| !s.is_empty()),
        author: str_at(u, "name").map(str::to_owned),
        station_name: None,
        artwork_url: picture(u),
        web_url: Some(format!("{SITE}/{user}/")),
        feed_url: None,
        episode_count: u32_at(u, "cloudcast_count"),
        show_id: None,
        broadcaster: None,
    })
}

/// The account in a Mixcloud link (`mixcloud.com/{user}/` or one of its uploads).
pub fn parse_link(link: &str) -> Option<String> {
    let url = url::Url::parse(link.trim()).ok()?;
    if !matches!(
        url.host_str()?,
        "mixcloud.com" | "www.mixcloud.com" | "m.mixcloud.com"
    ) {
        return None;
    }
    let user = url.path_segments()?.next()?;
    (is_slug(user) && !matches!(user, "discover" | "upload" | "search" | "genres")).then(|| user.to_owned())
}

fn show_data(user: &Value, casts: &[Value]) -> Result<ShowData, ProviderError> {
    let name = user_of(str_at(user, "key").unwrap_or_default())
        .ok_or_else(|| ProviderError::NotFound("Mixcloud has no such account".into()))?
        .to_owned();
    let episodes: Vec<EpisodeData> = casts
        .iter()
        .filter_map(|c| {
            // `/{user}/{slug}/` without the slashes: `{user}/{slug}`.
            let key = str_at(c, "key")?.trim_matches('/');
            let (u, slug) = key.split_once('/')?;
            (is_slug(u) && is_slug(slug)).then_some(())?;
            let tags: Vec<&str> = c
                .get("tags")
                .and_then(Value::as_array)
                .map(|t| t.iter().filter_map(|x| str_at(x, "name")).collect())
                .unwrap_or_default();
            Some(EpisodeData {
                external_id: key.to_owned(),
                series_external_id: "all".to_owned(),
                number: None,
                title: str_at(c, "name").unwrap_or("Untitled").trim().to_owned(),
                synopsis: (!tags.is_empty()).then(|| format!("Tags: {}.", tags.join(", "))),
                first_broadcast: str_at(c, "created_time")
                    .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                    .map(|d| d.with_timezone(&Utc)),
                duration_secs: u32_at(c, "audio_length"),
                enclosure_url: None,
                guid: None,
            })
        })
        .collect();
    Ok(ShowData {
        external_id: name.clone(),
        title: str_at(user, "name").unwrap_or(&name).trim().to_owned(),
        synopsis: str_at(user, "biog").map(plain_text).filter(|s| !s.is_empty()),
        author: str_at(user, "name").map(str::to_owned),
        web_url: Some(format!("{SITE}/{name}/")),
        artwork_url: picture(user),
        first_broadcast: episodes.iter().filter_map(|e| e.first_broadcast).min(),
        series: vec![SeriesData {
            external_id: "all".to_owned(),
            ..SeriesData::default()
        }],
        episodes,
        complete: true,
        ..ShowData::default()
    })
}

#[async_trait]
impl RadioProvider for MixcloudProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Mixcloud"
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn default_on(&self) -> bool {
        false
    }

    fn episodes_arrive(&self) -> &'static str {
        "Catch-up: each upload's audio is fetched from its Mixcloud page when catch-up is on"
    }

    fn risk_note(&self) -> Option<&'static str> {
        Some(
            "Mixcloud's terms allow streaming on its own site and apps only. Downloads may break them; for your personal use only.",
        )
    }

    fn has_catchup(&self) -> bool {
        true
    }

    fn catchup_url(&self, episode_external_id: &str) -> Option<String> {
        let (u, slug) = episode_external_id.split_once('/')?;
        (is_slug(u) && is_slug(slug)).then(|| format!("{SITE}/{u}/{slug}/"))
    }

    async fn search(&self, query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let url = url::Url::parse_with_params(
            &format!("{}/search/", self.base),
            &[("q", query.trim()), ("type", "user"), ("limit", "25")],
        )
        .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let data = self.api.get(url.as_str(), HeaderMap::new()).await?;
        Ok(data
            .get("data")
            .and_then(Value::as_array)
            .map(|d| d.iter().filter_map(user_hit).collect())
            .unwrap_or_default())
    }

    fn discover_lists(&self) -> Vec<DiscoverList> {
        TAGS.iter()
            .map(|(t, n)| DiscoverList::category(&format!("tag-{t}"), n))
            .collect()
    }

    async fn discover(&self, list: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let tag = list
            .strip_prefix("tag-")
            .filter(|t| TAGS.iter().any(|(x, _)| x == t))
            .ok_or_else(|| ProviderError::NotFound(format!("Mixcloud has no Discover list {list}")))?;
        // The tag's popular uploads, reduced to the accounts behind them.
        let data = self
            .api
            .get_for(
                &format!("{}/discover/{tag}/popular/?limit=50", self.base),
                HeaderMap::new(),
                DISCOVER_TTL,
            )
            .await?;
        let mut seen = std::collections::HashSet::new();
        Ok(data
            .get("data")
            .and_then(Value::as_array)
            .map(|d| {
                d.iter()
                    .filter_map(|c| c.get("user"))
                    .filter_map(user_hit)
                    .filter(|h| seen.insert(h.external_id.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        let Some(user) = parse_link(link) else {
            return Ok(None);
        };
        self.show(
            &user,
            &ShowFetch {
                quick: true,
                ..ShowFetch::default()
            },
        )
        .await
        .map(Some)
    }

    async fn show(&self, external_id: &str, fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let user = external_id.trim();
        if !is_slug(user) {
            return Err(ProviderError::NotFound(format!(
                "{user} is not a Mixcloud account"
            )));
        }
        let account = self
            .api
            .get(&format!("{}/{user}/", self.base), HeaderMap::new())
            .await?;
        let limit = if fetch.quick { 12 } else { MAX_EPISODES };
        let mut casts: Vec<Value> = Vec::new();
        let mut next = Some(format!("{}/{user}/cloudcasts/?limit={PAGE}", self.base));
        while let Some(url) = next.take() {
            let page = self.api.get(&url, HeaderMap::new()).await?;
            casts.extend(
                page.get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            if casts.len() >= limit {
                break;
            }
            // Only the API's own next page, never another host.
            next = page
                .get("paging")
                .and_then(|p| str_at(p, "next"))
                .filter(|n| n.starts_with(&self.base) || n.starts_with(BASE))
                .map(|n| n.replacen(BASE, &self.base, 1));
        }
        casts.truncate(limit);
        show_data(&account, &casts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_catchup_pages() {
        assert_eq!(
            parse_link("https://www.mixcloud.com/worldwidefm/").as_deref(),
            Some("worldwidefm")
        );
        assert_eq!(
            parse_link("https://www.mixcloud.com/worldwidefm/nossa-12-11-26/").as_deref(),
            Some("worldwidefm")
        );
        assert_eq!(parse_link("https://www.mixcloud.com/discover/jazz/"), None);
        assert_eq!(parse_link("https://evil.example/worldwidefm/"), None);
        let p = MixcloudProvider::new(reqwest::Client::new());
        assert_eq!(
            p.catchup_url("worldwidefm/nossa-12-11-26").as_deref(),
            Some("https://www.mixcloud.com/worldwidefm/nossa-12-11-26/")
        );
        assert_eq!(p.catchup_url("worldwidefm"), None);
        assert!(p.risk_note().is_some());
    }

    #[test]
    fn uploads_become_episodes() {
        let user: Value =
            serde_json::from_str(r#"{"key":"/worldwidefm/","name":"Worldwide FM","biog":"Broadcasting"}"#)
                .unwrap();
        let casts: Vec<Value> = serde_json::from_str(
            r#"[{"key":"/worldwidefm/nossa-12-11-26/","name":"NOSSA! // 12-11-26","created_time":"2026-11-12T15:00:00Z",
                 "audio_length":3601,"tags":[{"name":"MPB"}]}]"#,
        )
        .unwrap();
        let d = show_data(&user, &casts).unwrap();
        assert_eq!(d.external_id, "worldwidefm");
        assert_eq!(d.episodes[0].external_id, "worldwidefm/nossa-12-11-26");
        assert_eq!(d.episodes[0].duration_secs, Some(3601));
        assert_eq!(d.episodes[0].synopsis.as_deref(), Some("Tags: MPB."));
    }
}
