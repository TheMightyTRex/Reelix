//! BBC programmes: brands, series and episodes from the BBC's programme pages.
//!
//! **Undocumented endpoints.** The BBC publishes no API for this. The provider reads the
//! public JSON behind its programme pages (`/programmes/{pid}.json`,
//! `/programmes/{pid}/children.json`) and BBC Sounds search
//! (`rms.api.bbc.co.uk/v2/programmes/search/container`). Either can change without notice:
//! every read is checked, and a changed or missing answer fails with a plain message; saved
//! series and episodes are kept (a failed read changes nothing, and nothing is ever deleted).
//! Responses are cached for an hour (searches for five minutes), requests are spaced 1.5 s
//! apart and `Retry-After` is honoured.
//!
//! A show is a brand, or a series with no brand above it. Any pid inside a show (an episode,
//! a series) resolves to it by walking up to six parent levels. The brand's children are its
//! series and, sometimes, episodes listed straight under the brand: those become the
//! specials. A series inside a series is folded into its parent (and numbered by broadcast
//! order, since positions repeat across the folded series). Episode lengths need one request
//! each (the episode's `versions`), so at most [`ShowFetch::max_lookups`] are read per
//! refresh, newest first; settled series are not read again.
//!
//! The endpoints, the hierarchy walk and the link patterns are adapted from Storyarr's BBC
//! adapter (`services/app/adapters/bbc_programmes.py` in
//! <https://github.com/TheMightyTRex/storyarr>), Copyright (c) 2026 Dewarr contributors,
//! used under the MIT License
//! (<https://github.com/TheMightyTRex/storyarr/blob/main/LICENSE>).

use std::time::Duration;

use chrono::{DateTime, Utc};
use reelix_api_types::radio::RadioSeriesKind;
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::provider::{
    EpisodeData, ProviderError, RadioProvider, SearchPrefs, SeriesData, ShowData, ShowFetch, ShowHit,
    StationData, async_trait,
};
use crate::providers::http::{JsonReader, str_at};

pub const ID: &str = "bbc";

pub const PROGRAMMES_BASE: &str = "https://www.bbc.co.uk";
pub const SEARCH_BASE: &str = "https://rms.api.bbc.co.uk";
/// Show artwork (square) by image pid.
pub const ARTWORK_URL: &str = "https://ichef.bbci.co.uk/images/ic/640x640/{pid}.jpg";
/// Station logos (SVG) by service id.
pub const LOGO_URL: &str = "https://sounds.files.bbci.co.uk/3.12.0/networks/{id}/blocks-colour_default.svg";

pub const REQUEST_INTERVAL: Duration = Duration::from_millis(1500);
pub const CACHE_TTL: Duration = Duration::from_secs(3600);
pub const SEARCH_TTL: Duration = Duration::from_secs(300);

const PAGE_SIZE: u32 = 50;
/// Pages of children read per programme (1000 children).
const MAX_PAGES: u32 = 20;
const MAX_PARENT_LEVELS: usize = 6;
/// Nested series folded into their parent at most this deep.
const MAX_FOLD_DEPTH: u32 = 2;

const CHANGED: &str =
    "the BBC programme data has changed, so Reelix cannot read it; saved series and episodes are kept";

pub struct BbcProvider {
    programmes: JsonReader,
    search: JsonReader,
    base: String,
    search_base: String,
}

impl BbcProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            programmes: JsonReader::new(http.clone(), "the BBC", REQUEST_INTERVAL, CACHE_TTL),
            search: JsonReader::new(http, "BBC Sounds search", REQUEST_INTERVAL, SEARCH_TTL),
            base: PROGRAMMES_BASE.to_owned(),
            search_base: SEARCH_BASE.to_owned(),
        }
    }

    /// Other hosts for the programme pages and the search (tests).
    pub fn with_base_urls(mut self, programmes: &str, search: &str) -> Self {
        self.base = programmes.trim_end_matches('/').to_owned();
        self.search_base = search.trim_end_matches('/').to_owned();
        self
    }

    /// Spacing between requests (tests use zero).
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.programmes = self.programmes.with_interval(interval);
        self.search = self.search.with_interval(interval);
        self
    }

    /// The `programme` object of `{pid}.json`, checked.
    async fn programme(&self, pid: &str) -> Result<Value, ProviderError> {
        let url = format!("{}/programmes/{pid}.json", self.base);
        let data = self.programmes.get(&url, HeaderMap::new()).await?;
        let item = data.get("programme").cloned().unwrap_or(Value::Null);
        let kind = str_at(&item, "type");
        if str_at(&item, "pid") != Some(pid) || !matches!(kind, Some("brand" | "series" | "episode" | "clip"))
        {
            return Err(ProviderError::Parse(CHANGED.into()));
        }
        Ok(item)
    }

    /// Every child of `pid` (pages of [`PAGE_SIZE`]).
    async fn children(&self, pid: &str) -> Result<Vec<Value>, ProviderError> {
        let mut found = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut url = format!("{}/programmes/{pid}/children.json?limit={PAGE_SIZE}", self.base);
            if page > 1 {
                url.push_str(&format!("&page={page}"));
            }
            let data = self.programmes.get(&url, HeaderMap::new()).await?;
            let listing = data.get("children");
            let items = listing
                .and_then(|l| l.get("programmes"))
                .and_then(Value::as_array);
            let total = listing.and_then(|l| l.get("total")).and_then(Value::as_u64);
            let (Some(items), Some(total)) = (items, total) else {
                return Err(ProviderError::Parse(CHANGED.into()));
            };
            found.extend(
                items
                    .iter()
                    .filter(|i| str_at(i, "pid").is_some_and(is_pid))
                    .cloned(),
            );
            if items.is_empty() || found.len() as u64 >= total {
                break;
            }
        }
        Ok(found)
    }

    /// The show's top programme for any pid in it (an episode resolves to its brand).
    async fn resolve(&self, pid: &str) -> Result<Value, ProviderError> {
        let item = self.programme(pid).await?;
        if str_at(&item, "type") == Some("brand") {
            return Ok(item);
        }
        match top_pid(&item) {
            Some(top) if top != pid => self.programme(&top).await,
            _ if str_at(&item, "type") == Some("series") => Ok(item),
            _ => Err(ProviderError::NotFound(
                "this is a single BBC programme, not part of a show; paste a link to the show instead".into(),
            )),
        }
    }

    /// Episodes of a series; a series inside it is folded in (positions dropped).
    async fn series_episodes(&self, pid: &str) -> Result<Vec<Value>, ProviderError> {
        let mut found = Vec::new();
        let mut nested = false;
        let mut todo = vec![(pid.to_owned(), 0u32)];
        while let Some((pid, depth)) = todo.pop() {
            for item in self.children(&pid).await? {
                match str_at(&item, "type") {
                    Some("episode") => found.push(item),
                    Some("series") if depth < MAX_FOLD_DEPTH => {
                        nested = true;
                        if let Some(p) = str_at(&item, "pid") {
                            todo.push((p.to_owned(), depth + 1));
                        }
                    }
                    _ => {}
                }
            }
        }
        if nested {
            for item in &mut found {
                if let Some(o) = item.as_object_mut() {
                    o.remove("position");
                }
            }
        }
        Ok(found)
    }

    /// Reads the show whose top programme is `top`.
    async fn read(&self, top: &Value, fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let mut show = show_base(top)?;
        let top_pid = show.external_id.clone();
        // (series external id, pid to read) newest first.
        let mut to_read: Vec<(String, String, Option<DateTime<Utc>>)> = Vec::new();
        if str_at(top, "type") == Some("series") {
            show.series.push(series_data(top));
            to_read.push((
                top_pid.clone(),
                top_pid.clone(),
                when(top, "first_broadcast_date"),
            ));
        } else {
            let kids = self.children(&top_pid).await?;
            let mut loose = Vec::new();
            for k in &kids {
                match str_at(k, "type") {
                    Some("series") => {
                        let s = series_data(k);
                        to_read.push((
                            s.external_id.clone(),
                            s.external_id.clone(),
                            when(k, "first_broadcast_date"),
                        ));
                        show.series.push(s);
                    }
                    Some("episode") => loose.push(k),
                    _ => {}
                }
            }
            if !loose.is_empty() {
                show.series.push(SeriesData {
                    external_id: SPECIALS.into(),
                    kind: RadioSeriesKind::Specials,
                    title: Some("Specials".into()),
                    ..SeriesData::default()
                });
                show.episodes
                    .extend(loose.into_iter().filter_map(|e| episode_data(e, SPECIALS)));
            }
        }
        // Settled series are left out entirely (not a removal).
        show.series
            .retain(|s| !fetch.skip_series.contains(&s.external_id));
        to_read.retain(|(sid, _, _)| !fetch.skip_series.contains(sid));
        to_read.sort_by_key(|(_, _, first)| std::cmp::Reverse(*first));
        if fetch.quick {
            // A first look: the newest series only; the service reads the rest.
            to_read.truncate(1);
            show.complete = false;
        }
        for (sid, pid, _) in &to_read {
            match self.series_episodes(pid).await {
                Ok(items) => show
                    .episodes
                    .extend(items.iter().filter_map(|e| episode_data(e, sid))),
                Err(ProviderError::RateLimited { .. }) => {
                    // Keep what was read; nothing is marked removed on a partial read.
                    tracing::info!(show = %show.title, "BBC asked Reelix to slow down; the rest is read next time");
                    show.complete = false;
                    break;
                }
                Err(ProviderError::NotFound(_)) => show.complete = false,
                Err(e) => return Err(e),
            }
        }
        if !fetch.quick {
            self.lengths(&mut show, fetch).await;
        }
        Ok(show)
    }

    /// Reads up to `max_lookups` episode lengths, newest broadcast first, for episodes whose
    /// length is not saved yet. Lengths are a nicety: a missing page is skipped, a rate limit
    /// stops the lookups.
    async fn lengths(&self, show: &mut ShowData, fetch: &ShowFetch) {
        let now = Utc::now();
        let mut due: Vec<(DateTime<Utc>, usize)> = show
            .episodes
            .iter()
            .enumerate()
            .filter(|(_, e)| !fetch.known_episodes.contains(&e.external_id))
            .filter_map(|(i, e)| e.first_broadcast.filter(|d| *d <= now).map(|d| (d, i)))
            .collect();
        due.sort_by_key(|(d, _)| std::cmp::Reverse(*d));
        for (_, i) in due.into_iter().take(fetch.max_lookups as usize) {
            let pid = show.episodes[i].external_id.clone();
            match self.programme(&pid).await {
                Ok(item) => show.episodes[i].duration_secs = duration(&item),
                Err(ProviderError::RateLimited { .. }) => break,
                Err(e) => tracing::debug!(pid, error = %e, "BBC episode length not read"),
            }
        }
    }
}

/// External id of the series that holds episodes listed straight under a brand.
const SPECIALS: &str = "specials";

/// BBC pids: digits and consonants, 8 to 15 characters (`b006qpgr`, `m002vm5p`).
pub fn is_pid(s: &str) -> bool {
    (8..=15).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_lowercase() && !b"aeiou".contains(&b)))
}

fn is_service_id(s: &str) -> bool {
    (2..=60).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The pid in a BBC programmes or BBC Sounds link (or a bare pid); `None` for other links.
pub fn parse_link(link: &str) -> Option<String> {
    let link = link.trim();
    if is_pid(link) {
        return Some(link.to_owned());
    }
    let u = url::Url::parse(link).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if !matches!(u.scheme(), "http" | "https")
        || !matches!(
            host.as_str(),
            "bbc.co.uk" | "www.bbc.co.uk" | "bbc.com" | "www.bbc.com"
        )
    {
        return None;
    }
    let seg: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    let pid = match seg.as_slice() {
        ["programmes", pid, ..] => pid.trim_end_matches(".json"),
        ["sounds", "brand" | "series" | "play" | "episode", pid, ..] => pid,
        _ => return None,
    };
    is_pid(pid).then(|| pid.to_owned())
}

/// The brand above an episode or series, or its topmost series when there is no brand.
fn top_pid(item: &Value) -> Option<String> {
    let mut top = None;
    let mut node = item;
    for _ in 0..MAX_PARENT_LEVELS {
        let Some(parent) = node.get("parent").and_then(|p| p.get("programme")) else {
            break;
        };
        if matches!(str_at(parent, "type"), Some("brand" | "series"))
            && let Some(pid) = str_at(parent, "pid").filter(|p| is_pid(p))
        {
            top = Some(pid.to_owned());
        }
        node = parent;
    }
    top
}

fn when(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let s = str_at(v, key)?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn synopsis(v: &Value) -> Option<String> {
    ["long_synopsis", "medium_synopsis", "short_synopsis"]
        .iter()
        .find_map(|k| str_at(v, k))
        .map(str::to_owned)
}

fn position(v: &Value) -> Option<u32> {
    v.get("position")?
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

fn image_pid(v: &Value) -> Option<&str> {
    v.get("image")
        .and_then(|i| str_at(i, "pid"))
        .filter(|p| is_pid(p))
}

fn station(v: &Value) -> Option<StationData> {
    let service = v.get("ownership")?.get("service")?;
    let id = str_at(service, "id").filter(|i| is_service_id(i))?;
    let name = str_at(service, "title")?;
    Some(StationData {
        external_id: id.to_owned(),
        name: name.to_owned(),
        logo_url: Some(LOGO_URL.replace("{id}", id)),
    })
}

fn show_base(top: &Value) -> Result<ShowData, ProviderError> {
    let pid = str_at(top, "pid").ok_or_else(|| ProviderError::Parse(CHANGED.into()))?;
    let title = str_at(top, "title").ok_or_else(|| ProviderError::Parse(CHANGED.into()))?;
    Ok(ShowData {
        external_id: pid.to_owned(),
        title: title.to_owned(),
        synopsis: synopsis(top),
        author: None,
        web_url: Some(format!("https://www.bbc.co.uk/programmes/{pid}")),
        feed_url: None,
        artwork_url: image_pid(top).map(|p| ARTWORK_URL.replace("{pid}", p)),
        first_broadcast: when(top, "first_broadcast_date"),
        station: station(top),
        complete: true,
        ..ShowData::default()
    })
}

fn series_data(v: &Value) -> SeriesData {
    SeriesData {
        external_id: str_at(v, "pid").unwrap_or_default().to_owned(),
        kind: RadioSeriesKind::Series,
        number: position(v),
        title: str_at(v, "title").map(str::to_owned),
        synopsis: synopsis(v),
        expected_episodes: v
            .get("expected_child_count")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
    }
}

fn episode_data(v: &Value, series: &str) -> Option<EpisodeData> {
    let pid = str_at(v, "pid").filter(|p| is_pid(p))?;
    Some(EpisodeData {
        external_id: pid.to_owned(),
        series_external_id: series.to_owned(),
        number: position(v),
        title: str_at(v, "title").unwrap_or("Untitled episode").to_owned(),
        synopsis: synopsis(v),
        first_broadcast: when(v, "first_broadcast_date"),
        duration_secs: None,
        enclosure_url: None,
        guid: None,
    })
}

/// The episode's length from its `versions` (the canonical version first).
fn duration(item: &Value) -> Option<u32> {
    let mut versions: Vec<&Value> = item.get("versions")?.as_array()?.iter().collect();
    versions.sort_by_key(|v| v.get("canonical").and_then(Value::as_u64) != Some(1));
    versions
        .iter()
        .filter_map(|v| v.get("duration").and_then(Value::as_u64))
        .find(|s| (1..24 * 3600).contains(s))
        .and_then(|s| u32::try_from(s).ok())
}

/// Hits of BBC Sounds container search.
fn search_hits(data: &Value) -> Result<Vec<ShowHit>, ProviderError> {
    let items = data.get("data").and_then(Value::as_array).ok_or_else(|| {
        ProviderError::Parse("BBC Sounds search has changed; paste a link to the show instead".into())
    })?;
    Ok(items
        .iter()
        .filter(|i| str_at(i, "type") == Some("container_item"))
        .filter_map(|i| {
            let pid = str_at(i, "id").filter(|p| is_pid(p))?;
            let title = i.get("titles").and_then(|t| str_at(t, "primary"))?;
            let artwork_url = str_at(i, "image_url")
                .filter(|u| u.starts_with("https://ichef.bbci.co.uk/"))
                .map(|u| u.replace("{recipe}", "640x640"));
            Some(ShowHit {
                provider: ID.to_owned(),
                external_id: pid.to_owned(),
                title: title.to_owned(),
                synopsis: i
                    .get("synopses")
                    .and_then(|s| str_at(s, "short"))
                    .map(str::to_owned),
                author: None,
                station_name: i
                    .get("network")
                    .and_then(|n| str_at(n, "short_title"))
                    .map(str::to_owned),
                artwork_url,
                web_url: Some(format!("https://www.bbc.co.uk/programmes/{pid}")),
                feed_url: None,
                episode_count: None,
                show_id: None,
            })
        })
        .collect())
}

#[async_trait]
impl RadioProvider for BbcProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "BBC"
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn countries(&self) -> &'static [&'static str] {
        &["GB"]
    }

    fn languages(&self) -> &'static [&'static str] {
        &["en", "cy", "gd"]
    }

    async fn search(&self, query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let url = url::Url::parse_with_params(
            &format!("{}/v2/programmes/search/container", self.search_base),
            &[("q", query.trim())],
        )
        .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let data = self.search.get(url.as_str(), HeaderMap::new()).await?;
        search_hits(&data)
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        let Some(pid) = parse_link(link) else {
            return Ok(None);
        };
        let top = self.resolve(&pid).await?;
        let fetch = ShowFetch {
            quick: true,
            ..ShowFetch::default()
        };
        self.read(&top, &fetch).await.map(Some)
    }

    async fn show(&self, external_id: &str, fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let pid = external_id.trim();
        if !is_pid(pid) {
            return Err(ProviderError::NotFound(format!(
                "{pid} is not a BBC programme id"
            )));
        }
        let top = self.resolve(pid).await?;
        self.read(&top, fetch).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pids_and_links() {
        assert!(is_pid("b006s5dp"));
        assert!(is_pid("m002qrx3"));
        assert!(!is_pid("b006a5dp"), "vowels are not used");
        assert!(!is_pid("short"));
        assert_eq!(
            parse_link("https://www.bbc.co.uk/programmes/b006s5dp").as_deref(),
            Some("b006s5dp")
        );
        assert_eq!(
            parse_link("https://www.bbc.co.uk/programmes/b006s5dp/episodes/guide").as_deref(),
            Some("b006s5dp")
        );
        assert_eq!(
            parse_link("http://bbc.co.uk/programmes/b006s5dp.json").as_deref(),
            Some("b006s5dp")
        );
        assert_eq!(
            parse_link("https://www.bbc.co.uk/sounds/brand/b006s5dp").as_deref(),
            Some("b006s5dp")
        );
        assert_eq!(
            parse_link("https://www.bbc.co.uk/sounds/series/m002qrx3").as_deref(),
            Some("m002qrx3")
        );
        assert_eq!(
            parse_link("https://www.bbc.co.uk/sounds/play/m002rd20").as_deref(),
            Some("m002rd20")
        );
        assert_eq!(parse_link("b006s5dp").as_deref(), Some("b006s5dp"));
        assert_eq!(parse_link("https://evil.example/programmes/b006s5dp"), None);
        assert_eq!(parse_link("https://www.bbc.co.uk/news"), None);
        assert_eq!(parse_link("https://www.bbc.co.uk/sounds/brand/nope"), None);
    }

    #[test]
    fn lengths_prefer_the_canonical_version() {
        let v = serde_json::json!({"versions": [
            {"canonical": 0, "duration": 1720},
            {"canonical": 1, "duration": 1680},
        ]});
        assert_eq!(duration(&v), Some(1680));
        assert_eq!(duration(&serde_json::json!({"versions": []})), None);
        assert_eq!(duration(&serde_json::json!({})), None);
    }

    #[test]
    fn parents() {
        let ep = serde_json::json!({"type": "episode", "pid": "m002rd20", "parent": {"programme": {
            "type": "series", "pid": "m002qrx3", "parent": {"programme": {"type": "brand", "pid": "b006s5dp"}}
        }}});
        assert_eq!(top_pid(&ep).as_deref(), Some("b006s5dp"));
        let lone = serde_json::json!({"type": "episode", "pid": "m002rd20"});
        assert_eq!(top_pid(&lone), None);
    }
}
