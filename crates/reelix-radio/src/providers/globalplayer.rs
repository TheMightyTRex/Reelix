//! Global Player (RFX-97): catch-up shows of Global's stations (LBC, Capital, Heart, Smooth, Radio X, Gold, Classic
//! FM) from the undocumented JSON service behind globalplayer.com (`bff-web-guacamole.musicradio.com`).
//!
//! - A station's catch-up shows: `/globalplayer/catchups/{brand}/{area}`.
//! - A show and its episodes: `/globalplayer/catchups/{show id}`. Episodes stay for 7 days only, so a show is
//!   followed "from now on": what is already gone cannot be fetched.
//! - Audio: catch-up only (yt-dlp on the episode's page, [`crate::catchup`]). Off by default: Global's terms are
//!   for listening in its own apps.
//!
//! Ids carry the station (`{brand}.{area}.{id}`), so an episode's page can be built from its id alone.

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::provider::{
    DISCOVER_TTL, DiscoverList, EpisodeData, ProviderError, RadioProvider, SearchPrefs, SeriesData, ShowData,
    ShowFetch, ShowHit, StationData, async_trait,
};
use crate::providers::http::{JsonReader, str_at, url_at};
use crate::providers::rss::plain_text;

pub const ID: &str = "globalplayer";
pub const BASE: &str = "https://bff-web-guacamole.musicradio.com";
pub const SITE: &str = "https://www.globalplayer.com";
pub const CACHE_TTL: Duration = Duration::from_secs(1800);

/// The stations offered: `(brand, area, name)`. Links to Global's other stations work too.
pub const STATIONS: &[(&str, &str, &str)] = &[
    ("lbc", "uk", "LBC"),
    ("lbcnews", "uk", "LBC News"),
    ("capital", "uk", "Capital"),
    ("capitalxtra", "uk", "Capital XTRA"),
    ("heart", "uk", "Heart"),
    ("smooth", "uk", "Smooth"),
    ("radiox", "uk", "Radio X"),
    ("gold", "uk", "Gold"),
    ("classicfm", "uk", "Classic FM"),
];

pub struct GlobalPlayerProvider {
    api: JsonReader,
    base: String,
}

impl GlobalPlayerProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            api: JsonReader::new(http, "Global Player", Duration::from_millis(500), CACHE_TTL),
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

    async fn station_shows(&self, brand: &str, area: &str) -> Result<Vec<ShowHit>, ProviderError> {
        let data = self
            .api
            .get_for(
                &format!("{}/globalplayer/catchups/{brand}/{area}", self.base),
                HeaderMap::new(),
                DISCOVER_TTL,
            )
            .await?;
        let name = station_name(brand);
        Ok(data
            .as_array()
            .map(|shows| {
                shows
                    .iter()
                    .filter_map(|s| {
                        let id = str_at(s, "id").filter(|i| is_token(i))?;
                        Some(ShowHit {
                            provider: ID.to_owned(),
                            external_id: format!("{brand}.{area}.{id}"),
                            title: str_at(s, "title")?.trim().to_owned(),
                            synopsis: None,
                            author: Some(name.clone()),
                            station_name: Some(name.clone()),
                            artwork_url: url_at(s, "imageUrl"),
                            web_url: Some(format!("{SITE}/catchup/{brand}/{area}/{id}/")),
                            feed_url: None,
                            episode_count: None,
                            show_id: None,
                            broadcaster: Some(name.clone()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

fn is_token(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn is_slug(s: &str) -> bool {
    (1..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn station_name(brand: &str) -> String {
    STATIONS
        .iter()
        .find(|(b, _, _)| *b == brand)
        .map_or_else(|| brand.to_owned(), |(_, _, n)| (*n).to_owned())
}

/// `{brand}.{area}.{id}` split into its parts, each checked.
fn parts(id: &str) -> Option<(&str, &str, &str)> {
    let mut it = id.splitn(3, '.');
    let (brand, area, rest) = (it.next()?, it.next()?, it.next()?);
    (is_slug(brand) && is_slug(area) && is_token(rest)).then_some((brand, area, rest))
}

/// The show in a Global Player catch-up link (`globalplayer.com/catchup/{brand}/{area}/{show}/`), as an id.
pub fn parse_link(link: &str) -> Option<String> {
    let url = url::Url::parse(link.trim()).ok()?;
    if !matches!(url.host_str()?, "globalplayer.com" | "www.globalplayer.com") {
        return None;
    }
    let segs: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        ["catchup", brand, area, show] if *show != "episodes" => {
            let id = format!("{brand}.{area}.{show}");
            parts(&id).is_some().then_some(id)
        }
        _ => None,
    }
}

/// `3:00:00` or `45:00` as seconds.
fn duration(s: &str) -> Option<u32> {
    s.split(':')
        .try_fold(0u32, |acc, p| p.trim().parse::<u32>().ok().map(|n| acc * 60 + n))
}

fn show_data(brand: &str, area: &str, show: &Value) -> Result<ShowData, ProviderError> {
    let id = str_at(show, "id")
        .filter(|i| is_token(i))
        .ok_or_else(|| ProviderError::NotFound("Global Player has no such show".into()))?;
    let name = station_name(brand);
    let episodes: Vec<EpisodeData> = show
        .get("episodes")
        .and_then(Value::as_array)
        .map(|eps| {
            eps.iter()
                .filter_map(|e| {
                    let eid = str_at(e, "id").filter(|i| is_token(i))?;
                    let when = str_at(e, "startDate")
                        .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                        .map(|d| d.with_timezone(&Utc));
                    let title = match (str_at(e, "title"), when) {
                        (Some(t), Some(w)) => format!("{} – {}", t.trim(), w.format("%-d %B %Y")),
                        (Some(t), None) => t.trim().to_owned(),
                        (None, _) => "Untitled episode".to_owned(),
                    };
                    Some(EpisodeData {
                        external_id: format!("{brand}.{area}.{eid}"),
                        series_external_id: "all".to_owned(),
                        number: None,
                        title,
                        synopsis: str_at(e, "description").map(plain_text).filter(|s| !s.is_empty()),
                        first_broadcast: when,
                        duration_secs: str_at(e, "duration").and_then(duration),
                        enclosure_url: None,
                        guid: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ShowData {
        external_id: format!("{brand}.{area}.{id}"),
        title: str_at(show, "title").unwrap_or("Untitled show").trim().to_owned(),
        synopsis: str_at(show, "description")
            .map(plain_text)
            .filter(|s| !s.is_empty()),
        author: Some(name.clone()),
        web_url: Some(format!("{SITE}/catchup/{brand}/{area}/{id}/")),
        artwork_url: url_at(show, "imageUrl"),
        first_broadcast: episodes.iter().filter_map(|e| e.first_broadcast).min(),
        station: Some(StationData {
            external_id: format!("{brand}.{area}"),
            name,
            logo_url: None,
        }),
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
impl RadioProvider for GlobalPlayerProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Global Player"
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn countries(&self) -> &'static [&'static str] {
        &["GB"]
    }

    fn default_on(&self) -> bool {
        false
    }

    fn episodes_arrive(&self) -> &'static str {
        "Catch-up, kept for 7 days: follow from now on; each episode is fetched from its Global Player page when catch-up is on"
    }

    fn risk_note(&self) -> Option<&'static str> {
        Some(
            "Global's terms are for listening in its own apps, and episodes go after 7 days. Downloads are for your personal use only.",
        )
    }

    fn stations(&self) -> Vec<reelix_api_types::radio::RadioProviderStationDto> {
        STATIONS
            .iter()
            .map(|(b, a, n)| reelix_api_types::radio::RadioProviderStationDto {
                id: format!("{b}.{a}"),
                name: (*n).to_owned(),
                group: None,
                logo_url: None,
            })
            .collect()
    }

    fn has_catchup(&self) -> bool {
        true
    }

    fn catchup_url(&self, episode_external_id: &str) -> Option<String> {
        let (brand, area, id) = parts(episode_external_id)?;
        Some(format!("{SITE}/catchup/{brand}/{area}/episodes/{id}/"))
    }

    async fn search(&self, query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (brand, area, _) in STATIONS {
            match self.station_shows(brand, area).await {
                Ok(shows) => out.extend(shows.into_iter().filter(|h| {
                    let t = format!("{} {}", h.title, h.station_name.as_deref().unwrap_or_default())
                        .to_lowercase();
                    words.iter().all(|w| t.contains(w.as_str()))
                })),
                Err(e) => tracing::debug!(station = brand, error = %e, "Global Player station not read"),
            }
        }
        out.truncate(25);
        Ok(out)
    }

    fn discover_lists(&self) -> Vec<DiscoverList> {
        STATIONS
            .iter()
            .map(|(b, a, n)| {
                DiscoverList::category(&format!("station-{b}.{a}"), n).in_group("Global stations")
            })
            .collect()
    }

    async fn discover(&self, list: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let unknown = || ProviderError::NotFound(format!("Global Player has no Discover list {list}"));
        let (brand, area) = list
            .strip_prefix("station-")
            .and_then(|s| s.split_once('.'))
            .ok_or_else(unknown)?;
        STATIONS
            .iter()
            .find(|(b, a, _)| *b == brand && *a == area)
            .ok_or_else(unknown)?;
        self.station_shows(brand, area).await
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        let Some(id) = parse_link(link) else {
            return Ok(None);
        };
        self.show(
            &id,
            &ShowFetch {
                quick: true,
                ..ShowFetch::default()
            },
        )
        .await
        .map(Some)
    }

    async fn show(&self, external_id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let (brand, area, id) = parts(external_id.trim())
            .ok_or_else(|| ProviderError::NotFound(format!("{external_id} is not a Global Player show")))?;
        let show = self
            .api
            .get(
                &format!("{}/globalplayer/catchups/{id}", self.base),
                HeaderMap::new(),
            )
            .await?;
        show_data(brand, area, &show)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_ids_and_catchup_pages() {
        assert_eq!(
            parse_link("https://www.globalplayer.com/catchup/lbc/uk/b8FPjaW/").as_deref(),
            Some("lbc.uk.b8FPjaW")
        );
        assert_eq!(
            parse_link("https://www.globalplayer.com/catchup/lbc/uk/episodes/BUoaj2kK4yF2/"),
            None
        );
        assert_eq!(
            parse_link("https://www.globalplayer.com/catchup/LBC/../b8/"),
            None
        );
        let p = GlobalPlayerProvider::new(reqwest::Client::new());
        assert_eq!(
            p.catchup_url("lbc.uk.BUoaj2kK4yF2hCzgxSvo6ic9s").as_deref(),
            Some("https://www.globalplayer.com/catchup/lbc/uk/episodes/BUoaj2kK4yF2hCzgxSvo6ic9s/")
        );
        assert_eq!(p.catchup_url("lbc/uk/x"), None);
        assert_eq!(duration("3:00:00"), Some(10800));
        assert_eq!(duration("45:30"), Some(2730));
        assert!(!p.default_on());
    }

    #[test]
    fn a_show_and_its_week_of_episodes() {
        let show: Value = serde_json::from_str(
            r#"{"id":"b8FPjaW","title":"Ali Miraj","description":"Ali Miraj is Leading Britain's Conversation",
                "episodes":[{"id":"BUoaj2kK4yF2hCzgxSvo6ic9s","title":"Ali Miraj","duration":"3:00:00",
                             "startDate":"2026-11-08T12:00:00+00:00"}]}"#,
        )
        .unwrap();
        let d = show_data("lbc", "uk", &show).unwrap();
        assert_eq!(d.external_id, "lbc.uk.b8FPjaW");
        assert_eq!(d.station.as_ref().unwrap().name, "LBC");
        assert_eq!(d.episodes[0].external_id, "lbc.uk.BUoaj2kK4yF2hCzgxSvo6ic9s");
        assert_eq!(d.episodes[0].title, "Ali Miraj – 8 November 2026");
        assert_eq!(d.episodes[0].duration_secs, Some(10800));
    }
}
