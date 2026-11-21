//! Stremio subtitle add-ons: `GET {base}/subtitles/{type}/{id}.json`, with the file's hash,
//! size and name as extra arguments when known
//! (`{base}/subtitles/{type}/{id}/videoHash=…&videoSize=…&filename=….json`).
//!
//! Films are asked for by IMDb id (`tt0113277`), episodes by the series' IMDb id with season
//! and episode (`tt0903747:2:5`). The add-ons do not mark SDH or forced subtitles, so they are
//! used for normal subtitles only, and as matches by id: they do not say which subtitles were
//! made for the file.

use std::time::Duration;

use serde::Deserialize;
use url::Url;

use crate::error::{Error, Result};
use crate::matching::{Candidate, MatchLevel, Source};
use crate::naming::{Variant, language_matches};
use crate::pace::{Pacer, retry_after};

/// The add-on's base address from what was entered: `stremio://` becomes `https://`, and
/// `/manifest.json` and a trailing `/` are dropped.
pub fn addon_base(input: &str) -> Result<Url> {
    let mut s = input.trim().to_owned();
    if let Some(rest) = s.strip_prefix("stremio://") {
        s = format!("https://{rest}");
    }
    let s = s.trim_end_matches('/');
    let s = s
        .strip_suffix("/manifest.json")
        .unwrap_or(s)
        .trim_end_matches('/');
    let url = Url::parse(s).map_err(|e| Error::Settings(format!("add-on address {input}: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(Error::Settings(format!(
            "add-on address {input}: use an http or https address"
        )));
    }
    Ok(url)
}

/// What to ask an add-on for.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// `tt` IMDb id of the film, or of the series for an episode.
    pub imdb_id: String,
    pub episode: Option<(u32, u32)>,
    pub video_hash: Option<String>,
    pub video_size: Option<u64>,
    pub filename: Option<String>,
}

/// Percent-encodes everything but unreserved characters (`encodeURIComponent`).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The resource address for `req` under `base`.
pub fn resource_url(base: &Url, req: &Request) -> Result<Url> {
    let (kind, id) = match req.episode {
        Some((s, e)) => ("series", format!("{}:{s}:{e}", req.imdb_id)),
        None => ("movie", req.imdb_id.clone()),
    };
    let mut extra = Vec::new();
    if let Some(n) = &req.filename {
        extra.push(format!("filename={}", encode(n)));
    }
    if let Some(h) = &req.video_hash {
        extra.push(format!("videoHash={}", encode(h)));
    }
    if let Some(s) = req.video_size {
        extra.push(format!("videoSize={s}"));
    }
    let tail = if extra.is_empty() {
        format!("subtitles/{kind}/{}.json", encode(&id))
    } else {
        format!("subtitles/{kind}/{}/{}.json", encode(&id), extra.join("&"))
    };
    let mut base = base.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    Ok(base.join(&tail)?)
}

#[derive(Debug, Deserialize)]
pub struct SubtitlesResponse {
    #[serde(default)]
    pub subtitles: Vec<Item>,
}

#[derive(Debug, Deserialize)]
pub struct Item {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
}

/// Candidates from an add-on's answer, for the languages wanted (normal subtitles only). Items
/// without an `http(s)` address are left out.
pub fn candidates(resp: &SubtitlesResponse, addon: usize, languages: &[String]) -> Vec<Candidate> {
    resp.subtitles
        .iter()
        .filter_map(|item| {
            let url = item.url.as_deref()?;
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return None;
            }
            let label = item.lang.as_deref()?;
            let language = languages.iter().find(|l| language_matches(l, label))?;
            let release = item.id.as_ref().and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            });
            Some(Candidate {
                source: Source::Stremio(addon),
                source_id: url.to_owned(),
                language: language.clone(),
                variant: Variant::Normal,
                level: MatchLevel::Id,
                release,
                downloads: 0,
                machine_translated: false,
            })
        })
        .collect()
}

/// One add-on, with its own request spacing (a second apart).
#[derive(Debug)]
pub struct StremioAddon {
    http: reqwest::Client,
    pub base: Url,
    pacer: Pacer,
}

impl StremioAddon {
    pub fn new(http: reqwest::Client, base: Url) -> Self {
        Self {
            http,
            base,
            pacer: Pacer::new(Duration::from_secs(1)),
        }
    }

    pub fn with_gap(mut self, gap: Duration) -> Self {
        self.pacer = Pacer::new(gap);
        self
    }

    pub async fn subtitles(&self, req: &Request) -> Result<SubtitlesResponse> {
        let url = resource_url(&self.base, req)?;
        for attempt in 0..2 {
            self.pacer.wait().await;
            let resp = self.http.get(url.clone()).send().await?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt == 0 {
                self.pacer.back_off(retry_after(resp.headers())).await;
                continue;
            }
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(SubtitlesResponse { subtitles: vec![] });
            }
            return Ok(resp.error_for_status()?.json().await?);
        }
        Err(Error::RateLimited(self.base.to_string()))
    }

    pub async fn download(&self, link: &str, max_bytes: usize) -> Result<Vec<u8>> {
        self.pacer.wait().await;
        crate::opensubtitles::fetch_limited(&self.http, link, max_bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_addresses_are_tidied() {
        assert_eq!(
            addon_base("https://addon.example/abc/manifest.json")
                .unwrap()
                .as_str(),
            "https://addon.example/abc"
        );
        assert_eq!(
            addon_base("stremio://addon.example/manifest.json")
                .unwrap()
                .as_str(),
            "https://addon.example/"
        );
        assert!(addon_base("ftp://addon.example").is_err());
        assert!(addon_base("not an address").is_err());
    }

    #[test]
    fn resource_addresses() {
        let base = addon_base("https://addon.example/cfg").unwrap();
        let film = Request {
            imdb_id: "tt0113277".into(),
            episode: None,
            video_hash: None,
            video_size: None,
            filename: None,
        };
        assert_eq!(
            resource_url(&base, &film).unwrap().as_str(),
            "https://addon.example/cfg/subtitles/movie/tt0113277.json"
        );
        let ep = Request {
            imdb_id: "tt0903747".into(),
            episode: Some((2, 5)),
            video_hash: Some("8e245d9679d31e12".into()),
            video_size: Some(12_909_756),
            filename: Some("Show - S02E05 - Name.mkv".into()),
        };
        assert_eq!(
            resource_url(&base, &ep).unwrap().as_str(),
            "https://addon.example/cfg/subtitles/series/tt0903747%3A2%3A5/filename=Show%20-%20S02E05%20-%20Name.mkv&videoHash=8e245d9679d31e12&videoSize=12909756.json"
        );
    }

    #[test]
    fn parses_a_recorded_answer() {
        let raw = include_str!("../tests/fixtures/stremio-subtitles.json")
            .replace("{{BASE}}", "https://addon.example");
        let resp: SubtitlesResponse = serde_json::from_str(&raw).unwrap();
        assert_eq!(resp.subtitles.len(), 6);
        let c = candidates(&resp, 0, &["en".into(), "pt-BR".into()]);
        let ids: Vec<_> = c.iter().map(|c| c.source_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "https://addon.example/files/1001.srt",
                "https://addon.example/files/1002.srt",
                "https://addon.example/files/1004.srt",
            ]
        );
        assert!(
            c.iter()
                .all(|c| c.variant == Variant::Normal && c.level == MatchLevel::Id)
        );
        assert_eq!(c[2].language, "pt-BR");
    }
}
