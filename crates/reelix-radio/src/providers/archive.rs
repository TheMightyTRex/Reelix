//! The Internet Archive (<https://archive.org>): old-time radio and other radio programmes.
//!
//! Search uses the advanced search API (`/advancedsearch.php`) limited to audio items in
//! radio collections (`mediatype:audio AND (collection:oldtimeradio OR subject:radio)`). A show
//! is one item (`archive.org/details/{identifier}`): `/metadata/{identifier}` gives its title,
//! description and files, and each audio file is an episode (one per original recording: an
//! MP3 derived from a FLAC is the same episode), in series 1, numbered by its `track` when the
//! item has one, else in file order. Lengths come from the file's `length`.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::provider::{
    EpisodeData, ProviderError, RadioProvider, SearchPrefs, SeriesData, ShowData, ShowFetch, ShowHit,
    async_trait,
};
use crate::providers::http::{JsonReader, str_at};
use crate::providers::rss::{parse_duration, plain_text};

pub const ID: &str = "archive";

pub const BASE: &str = "https://archive.org";
pub const CACHE_TTL: Duration = Duration::from_secs(3600);
pub const REQUEST_INTERVAL: Duration = Duration::from_millis(500);
/// Which items count as radio.
pub const RADIO_FILTER: &str = "mediatype:audio AND (collection:oldtimeradio OR subject:radio)";
const SEARCH_ROWS: u32 = 25;
/// Item metadata can be large (thousands of files).
const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const SYNOPSIS_CHARS: usize = 600;

const AUDIO_EXTS: &[&str] = &["mp3", "ogg", "opus", "m4a", "flac", "wav"];

pub struct ArchiveProvider {
    api: JsonReader,
    base: String,
}

impl ArchiveProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            api: JsonReader::new(http, "the Internet Archive", REQUEST_INTERVAL, CACHE_TTL)
                .with_max_bytes(MAX_METADATA_BYTES),
            base: BASE.to_owned(),
        }
    }

    /// Another host (tests).
    pub fn with_base_url(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_owned();
        self
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.api = self.api.with_interval(interval);
        self
    }
}

/// Archive identifiers: letters, digits, `.`, `-` and `_`.
pub fn is_identifier(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// The identifier in an `archive.org/details/{identifier}` link.
pub fn parse_link(link: &str) -> Option<String> {
    let u = url::Url::parse(link.trim()).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if !matches!(u.scheme(), "http" | "https") || !matches!(host.as_str(), "archive.org" | "www.archive.org")
    {
        return None;
    }
    let seg: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    match seg.as_slice() {
        ["details", id, ..] if is_identifier(id) => Some((*id).to_owned()),
        _ => None,
    }
}

/// The search words without query syntax (brackets, quotes, operators).
fn search_terms(q: &str) -> String {
    q.chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| !matches!(*w, "AND" | "OR" | "NOT"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A metadata value that may be a string or a list of strings.
fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|s| !s.is_empty())
            .map(str::to_owned),
        _ => None,
    }
}

fn short(s: String) -> String {
    if s.chars().count() <= SYNOPSIS_CHARS {
        return s;
    }
    let mut t: String = s.chars().take(SYNOPSIS_CHARS).collect();
    t.push('…');
    t
}

/// `1948`, `1948-11-27`, `1948-11-27T00:00:00Z`.
fn date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    let day = s
        .get(..10)
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let day = day.or_else(|| {
        s.get(..4)
            .filter(|y| y.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|y| NaiveDate::from_ymd_opt(y.parse().ok()?, 1, 1))
    })?;
    Some(day.and_hms_opt(0, 0, 0)?.and_utc())
}

fn is_audio(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| AUDIO_EXTS.iter().any(|a| ext.eq_ignore_ascii_case(a)))
}

fn hit(base: &str, d: &Value) -> Option<ShowHit> {
    let id = str_at(d, "identifier").filter(|i| is_identifier(i))?;
    Some(ShowHit {
        provider: ID.to_owned(),
        external_id: id.to_owned(),
        title: text(d, "title").unwrap_or_else(|| id.to_owned()),
        synopsis: text(d, "description")
            .map(|s| short(plain_text(&s)))
            .filter(|s| !s.is_empty()),
        author: text(d, "creator"),
        station_name: None,
        artwork_url: Some(format!("{base}/services/img/{id}")),
        web_url: Some(format!("https://archive.org/details/{id}")),
        feed_url: None,
        episode_count: None,
        show_id: None,
    })
}

/// The show from an item's `/metadata` answer.
fn show_data(base: &str, id: &str, data: &Value) -> Result<ShowData, ProviderError> {
    let meta = data
        .get("metadata")
        .filter(|m| m.is_object())
        .ok_or_else(|| ProviderError::NotFound(format!("the Internet Archive has no item {id}")))?;
    let files = data
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // One episode per original recording: prefer an MP3 of it, else the original itself.
    let mut picked: Vec<(String, &Value)> = Vec::new();
    for f in &files {
        let Some(name) = str_at(f, "name").filter(|n| is_audio(n)) else {
            continue;
        };
        let group = str_at(f, "original")
            .filter(|_| str_at(f, "source") == Some("derivative"))
            .unwrap_or(name)
            .to_owned();
        let is_mp3 = name.to_ascii_lowercase().ends_with(".mp3");
        match picked.iter_mut().find(|(g, _)| *g == group) {
            Some(slot) => {
                let cur_mp3 =
                    str_at(slot.1, "name").is_some_and(|n| n.to_ascii_lowercase().ends_with(".mp3"));
                if is_mp3 && !cur_mp3 {
                    slot.1 = f;
                }
            }
            None => picked.push((group, f)),
        }
    }
    if picked.is_empty() {
        return Err(ProviderError::NotFound(format!(
            "the Internet Archive item {id} has no audio files"
        )));
    }
    let item_date = text(meta, "date").and_then(|d| date(&d));
    let episodes = picked
        .into_iter()
        .filter_map(|(_, f)| {
            let name = str_at(f, "name")?;
            let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
            let title = str_at(f, "title")
                .map(|t| t.replace('_', " "))
                .unwrap_or_else(|| stem.replace('_', " "));
            let encoded: String = name
                .split('/')
                .map(|p| {
                    url::form_urlencoded::byte_serialize(p.as_bytes())
                        .collect::<String>()
                        .replace('+', "%20")
                })
                .collect::<Vec<_>>()
                .join("/");
            Some(EpisodeData {
                external_id: name.to_owned(),
                series_external_id: "season:1".into(),
                number: str_at(f, "track")
                    .and_then(|t| t.split('/').next())
                    .and_then(|t| t.trim().parse::<u32>().ok())
                    .filter(|n| *n > 0),
                title: title.split_whitespace().collect::<Vec<_>>().join(" "),
                synopsis: None,
                first_broadcast: None,
                duration_secs: str_at(f, "length").and_then(parse_duration),
                enclosure_url: Some(format!("{base}/download/{id}/{encoded}")),
                guid: None,
            })
        })
        .collect();
    Ok(ShowData {
        external_id: id.to_owned(),
        title: text(meta, "title").unwrap_or_else(|| id.to_owned()),
        synopsis: text(meta, "description")
            .map(|s| plain_text(&s))
            .filter(|s| !s.is_empty()),
        author: text(meta, "creator"),
        web_url: Some(format!("https://archive.org/details/{id}")),
        feed_url: None,
        artwork_url: Some(format!("{base}/services/img/{id}")),
        first_broadcast: item_date,
        station: None,
        series: vec![SeriesData {
            external_id: "season:1".into(),
            ..SeriesData::default()
        }],
        episodes,
        complete: true,
        ..ShowData::default()
    })
}

#[async_trait]
impl RadioProvider for ArchiveProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Internet Archive"
    }

    fn supports_search(&self) -> bool {
        true
    }

    async fn search(&self, query: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        let terms = search_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let q = format!("({terms}) AND {RADIO_FILTER}");
        let rows = SEARCH_ROWS.to_string();
        let url = url::Url::parse_with_params(
            &format!("{}/advancedsearch.php", self.base),
            &[
                ("q", q.as_str()),
                ("fl[]", "identifier"),
                ("fl[]", "title"),
                ("fl[]", "description"),
                ("fl[]", "creator"),
                ("rows", rows.as_str()),
                ("output", "json"),
            ],
        )
        .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let data = self.api.get(url.as_str(), HeaderMap::new()).await?;
        let docs = data
            .get("response")
            .and_then(|r| r.get("docs"))
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::Parse("the Internet Archive sent an unexpected answer".into()))?;
        Ok(docs.iter().filter_map(|d| hit(&self.base, d)).collect())
    }

    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError> {
        let Some(id) = parse_link(link) else {
            return Ok(None);
        };
        self.show(&id, &ShowFetch::default()).await.map(Some)
    }

    async fn show(&self, external_id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        let id = external_id.trim();
        if !is_identifier(id) {
            return Err(ProviderError::NotFound(format!(
                "{id} is not an Internet Archive identifier"
            )));
        }
        let data = self
            .api
            .get(&format!("{}/metadata/{id}", self.base), HeaderMap::new())
            .await?;
        show_data(&self.base, id, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_terms() {
        assert_eq!(
            parse_link("https://archive.org/details/OTRR_Dragnet").as_deref(),
            Some("OTRR_Dragnet")
        );
        assert_eq!(
            parse_link("https://archive.org/details/OTRR_Dragnet/file.mp3").as_deref(),
            Some("OTRR_Dragnet")
        );
        assert_eq!(parse_link("https://archive.org/search?query=x"), None);
        assert_eq!(parse_link("https://example.org/details/x"), None);
        assert_eq!(search_terms("dragnet) OR (collection:*"), "dragnet collection");
    }

    #[test]
    fn dates() {
        assert_eq!(date("1948").unwrap().to_rfc3339(), "1948-01-01T00:00:00+00:00");
        assert_eq!(
            date("1948-11-27").unwrap().to_rfc3339(),
            "1948-11-27T00:00:00+00:00"
        );
        assert!(date("someday").is_none());
    }
}
