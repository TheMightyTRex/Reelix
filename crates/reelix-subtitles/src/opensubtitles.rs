//! OpenSubtitles.com REST API (v1) with the person's own API key and account.
//!
//! `GET /subtitles` finds subtitles by file hash, IMDb or TMDb id (with season and episode) or
//! name; `POST /login` signs in for the account's daily download allowance; `POST /download`
//! gives a short-lived link to the file. Requests are spaced a second apart (the API allows
//! five a second and one sign-in a second), a "slow down" answer is waited out once, and an
//! empty download allowance stops downloads until the next run.

use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;
use url::Url;

use crate::error::{Error, Result};
use crate::matching::{Candidate, Source, level_of};
use crate::naming::Variant;
use crate::pace::{Pacer, retry_after};

pub const DEFAULT_BASE: &str = "https://api.opensubtitles.com/api/v1/";
/// A sign-in lasts 24 hours; Reelix signs in again after 23.
const TOKEN_LIFE: Duration = Duration::from_secs(23 * 3600);

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub moviehash: Option<String>,
    /// Film: its IMDb id; episode: the series' (sent as the parent id).
    pub imdb_id: Option<u64>,
    /// Film: its TMDb id; episode: the series'.
    pub tmdb_id: Option<u64>,
    /// Season and episode numbers, for an episode.
    pub episode: Option<(u32, u32)>,
    /// Title, when there is no id.
    pub title: Option<String>,
    pub year: Option<i32>,
    /// Language codes from the settings.
    pub languages: Vec<String>,
}

impl Query {
    fn has_id(&self) -> bool {
        self.imdb_id.is_some() || self.tmdb_id.is_some()
    }

    /// Query parameters, lower case and in alphabetical order as the API asks (other orders
    /// are answered with a redirect).
    pub fn params(&self) -> Vec<(&'static str, String)> {
        let mut p: Vec<(&'static str, String)> = Vec::new();
        let mut langs: Vec<String> = self.languages.iter().map(|l| os_language(l)).collect();
        langs.sort();
        langs.dedup();
        if let Some((s, e)) = self.episode {
            p.push(("episode_number", e.to_string()));
            if let Some(id) = self.imdb_id {
                p.push(("parent_imdb_id", id.to_string()));
            }
            if let Some(id) = self.tmdb_id {
                p.push(("parent_tmdb_id", id.to_string()));
            }
            p.push(("season_number", s.to_string()));
        } else {
            if let Some(id) = self.imdb_id {
                p.push(("imdb_id", id.to_string()));
            }
            if let Some(id) = self.tmdb_id {
                p.push(("tmdb_id", id.to_string()));
            }
        }
        p.push(("languages", langs.join(",")));
        if let Some(h) = &self.moviehash {
            p.push(("moviehash", h.to_lowercase()));
        }
        if !self.has_id()
            && let Some(t) = &self.title
        {
            p.push(("query", t.to_lowercase()));
        }
        p.push((
            "type",
            if self.episode.is_some() {
                "episode"
            } else {
                "movie"
            }
            .into(),
        ));
        if !self.has_id()
            && let Some(y) = self.year
        {
            p.push(("year", y.to_string()));
        }
        p.sort_by_key(|(k, _)| *k);
        p
    }
}

/// OpenSubtitles' code for a language from the settings: plain Portuguese and Chinese are
/// `pt-PT` and `zh-CN` there.
pub fn os_language(code: &str) -> String {
    match code {
        "pt" => "pt-pt".into(),
        "zh" => "zh-cn".into(),
        other => other.to_lowercase(),
    }
}

/// The settings language an OpenSubtitles language label belongs to.
fn settings_language<'a>(label: &str, wanted: &'a [String]) -> Option<&'a String> {
    wanted.iter().find(|w| os_language(w).eq_ignore_ascii_case(label))
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub data: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
pub struct SearchItem {
    pub id: String,
    pub attributes: Attributes,
}

#[derive(Debug, Deserialize)]
pub struct Attributes {
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub download_count: u64,
    #[serde(default)]
    pub hearing_impaired: bool,
    #[serde(default)]
    pub foreign_parts_only: bool,
    #[serde(default)]
    pub ai_translated: bool,
    #[serde(default)]
    pub machine_translated: bool,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub moviehash_match: bool,
    #[serde(default)]
    pub files: Vec<FileRef>,
}

#[derive(Debug, Deserialize)]
pub struct FileRef {
    pub file_id: u64,
    #[serde(default)]
    pub cd_number: Option<u32>,
    #[serde(default)]
    pub file_name: Option<String>,
}

/// Candidates from a search answer: one per subtitle with a single file (subtitles split over
/// several CDs are left out), classed as normal, SDH or forced.
pub fn candidates(resp: &SearchResponse, query: &Query, file_release: Option<&str>) -> Vec<Candidate> {
    resp.data
        .iter()
        .filter_map(|item| {
            let a = &item.attributes;
            let [file] = a.files.as_slice() else {
                return None;
            };
            let language = settings_language(a.language.as_deref()?, &query.languages)?;
            let level = level_of(
                a.moviehash_match && query.moviehash.is_some(),
                query.has_id(),
                a.release.as_deref(),
                file_release,
            )?;
            let variant = if a.foreign_parts_only {
                Variant::Forced
            } else if a.hearing_impaired {
                Variant::Sdh
            } else {
                Variant::Normal
            };
            Some(Candidate {
                source: Source::OpenSubtitles,
                source_id: file.file_id.to_string(),
                language: language.clone(),
                variant,
                level,
                release: a.release.clone().or_else(|| file.file_name.clone()),
                downloads: a.download_count,
                machine_translated: a.ai_translated || a.machine_translated,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    token: String,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DownloadResponse {
    pub link: String,
    #[serde(default)]
    pub remaining: Option<i64>,
    #[serde(default)]
    pub reset_time_utc: Option<String>,
}

#[derive(Debug, Clone)]
struct Session {
    token: String,
    base: Url,
    at: tokio::time::Instant,
}

/// Account and key for the client.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub api_key: String,
    pub username: String,
    pub password: String,
}

/// OpenSubtitles client. One per server: it holds the sign-in and the request spacing.
#[derive(Debug)]
pub struct OpenSubtitles {
    http: reqwest::Client,
    base: Url,
    user_agent: String,
    pacer: Pacer,
    session: Mutex<Option<(String, Session)>>,
    /// Set when the download allowance ran out; downloads wait until a new run after it.
    exhausted_until: Mutex<Option<chrono::DateTime<chrono::Utc>>>,
}

impl OpenSubtitles {
    pub fn new(http: reqwest::Client, base: Url) -> Self {
        Self {
            http,
            base,
            user_agent: format!("Reelix v{}", env!("CARGO_PKG_VERSION")),
            pacer: Pacer::new(Duration::from_secs(1)),
            session: Mutex::new(None),
            exhausted_until: Mutex::new(None),
        }
    }

    /// Spacing for tests.
    pub fn with_gap(mut self, gap: Duration) -> Self {
        self.pacer = Pacer::new(gap);
        self
    }

    fn request(&self, method: reqwest::Method, url: Url, creds: &Credentials) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header("Api-Key", creds.api_key.trim())
            .header(reqwest::header::USER_AGENT, &self.user_agent)
            .header(reqwest::header::ACCEPT, "application/json")
    }

    /// Sends a request, waiting out one "slow down" answer.
    async fn send(&self, build: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response> {
        for attempt in 0..2 {
            self.pacer.wait().await;
            let resp = build().send().await?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt == 0 {
                self.pacer.back_off(retry_after(resp.headers())).await;
                continue;
            }
            return Ok(resp);
        }
        Err(Error::RateLimited("OpenSubtitles".into()))
    }

    /// The signed-in session (signing in when needed), or the plain address without an account.
    async fn session(&self, creds: &Credentials) -> Result<(Option<String>, Url)> {
        if creds.username.trim().is_empty() {
            return Ok((None, self.base.clone()));
        }
        let mut guard = self.session.lock().await;
        if let Some((user, s)) = guard.as_ref()
            && *user == creds.username
            && s.at.elapsed() < TOKEN_LIFE
        {
            return Ok((Some(s.token.clone()), s.base.clone()));
        }
        let url = self.base.join("login")?;
        let body = serde_json::json!({ "username": creds.username.trim(), "password": creds.password });
        let resp = self
            .send(|| {
                self.request(reqwest::Method::POST, url.clone(), creds)
                    .json(&body)
            })
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Source(
                "OpenSubtitles did not accept the username or password".into(),
            ));
        }
        let login: LoginResponse = checked(resp, "sign-in").await?.json().await?;
        // VIP accounts are given another OpenSubtitles host; only those are followed.
        let host = login
            .base_url
            .as_deref()
            .map(|h| h.trim().trim_start_matches("https://").trim_end_matches('/'))
            .filter(|h| h.ends_with(".opensubtitles.com") && !h.contains('/'));
        let base = match host {
            Some(host)
                if self.base.host_str() == Some("api.opensubtitles.com")
                    && Some(host) != self.base.host_str() =>
            {
                let mut b = self.base.clone();
                b.set_host(Some(host)).map_err(|e| Error::Source(e.to_string()))?;
                b
            }
            _ => self.base.clone(),
        };
        *guard = Some((
            creds.username.clone(),
            Session {
                token: login.token.clone(),
                base: base.clone(),
                at: tokio::time::Instant::now(),
            },
        ));
        Ok((Some(login.token), base))
    }

    /// Finds subtitles for `query`.
    pub async fn search(&self, creds: &Credentials, query: &Query) -> Result<SearchResponse> {
        let (_, base) = self.session(creds).await?;
        let mut url = base.join("subtitles")?;
        url.query_pairs_mut().extend_pairs(query.params());
        let resp = self
            .send(|| self.request(reqwest::Method::GET, url.clone(), creds))
            .await?;
        Ok(checked(resp, "search").await?.json().await?)
    }

    /// Downloads one subtitle file (at most `max_bytes`).
    pub async fn download(&self, creds: &Credentials, file_id: &str, max_bytes: usize) -> Result<Vec<u8>> {
        if let Some(until) = *self.exhausted_until.lock().await
            && chrono::Utc::now() < until
        {
            return Err(Error::QuotaExhausted);
        }
        let file_id: u64 = file_id
            .parse()
            .map_err(|_| Error::Source(format!("not an OpenSubtitles file id: {file_id}")))?;
        let (token, base) = self.session(creds).await?;
        let url = base.join("download")?;
        let body = serde_json::json!({ "file_id": file_id });
        let resp = self
            .send(|| {
                let r = self
                    .request(reqwest::Method::POST, url.clone(), creds)
                    .json(&body);
                match &token {
                    Some(t) => r.bearer_auth(t),
                    None => r,
                }
            })
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_ACCEPTABLE {
            self.mark_exhausted(None).await;
            return Err(Error::QuotaExhausted);
        }
        let d: DownloadResponse = checked(resp, "download").await?.json().await?;
        if d.remaining.is_some_and(|r| r <= 0) {
            self.mark_exhausted(d.reset_time_utc.as_deref()).await;
        }
        self.pacer.wait().await;
        fetch_limited(&self.http, &d.link, max_bytes).await
    }

    async fn mark_exhausted(&self, reset: Option<&str>) {
        let until = reset
            .and_then(|r| chrono::DateTime::parse_from_rfc3339(r).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(6));
        tracing::info!(%until, "OpenSubtitles download allowance used up");
        *self.exhausted_until.lock().await = Some(until);
    }
}

async fn checked(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let body: String = body.chars().take(200).collect();
    Err(Error::Source(format!(
        "OpenSubtitles {what} answered {status}: {body}"
    )))
}

/// Fetches an `http(s)` address, refusing bodies larger than `max_bytes`.
pub async fn fetch_limited(http: &reqwest::Client, link: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let url = Url::parse(link)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::Source(format!("not a web address: {link}")));
    }
    let mut resp = http.get(url).send().await?.error_for_status()?;
    if resp.content_length().is_some_and(|n| n as usize > max_bytes) {
        return Err(Error::Source("subtitle file too large".into()));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        out.extend_from_slice(&chunk);
        if out.len() > max_bytes {
            return Err(Error::Source("subtitle file too large".into()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::MatchLevel;

    const SEARCH: &str = include_str!("../tests/fixtures/opensubtitles-search.json");

    fn film_query() -> Query {
        Query {
            moviehash: Some("8e245d9679d31e12".into()),
            imdb_id: Some(113277),
            tmdb_id: Some(949),
            languages: vec!["en".into(), "pt".into()],
            ..Default::default()
        }
    }

    #[test]
    fn parameters_are_sorted_and_lower_case() {
        let p = film_query().params();
        let keys: Vec<_> = p.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["imdb_id", "languages", "moviehash", "tmdb_id", "type"]);
        assert_eq!(p[1].1, "en,pt-pt");
        let ep = Query {
            imdb_id: Some(903747),
            episode: Some((2, 5)),
            languages: vec!["en".into()],
            ..Default::default()
        }
        .params();
        assert_eq!(
            ep,
            vec![
                ("episode_number", "5".to_string()),
                ("languages", "en".into()),
                ("parent_imdb_id", "903747".into()),
                ("season_number", "2".into()),
                ("type", "episode".into()),
            ]
        );
        let by_name = Query {
            title: Some("Heat".into()),
            year: Some(1995),
            languages: vec!["en".into()],
            ..Default::default()
        }
        .params();
        assert!(by_name.contains(&("query", "heat".into())));
        assert!(by_name.contains(&("year", "1995".into())));
    }

    #[test]
    fn parses_a_recorded_search() {
        let resp: SearchResponse = serde_json::from_str(SEARCH).unwrap();
        assert_eq!(resp.data.len(), 6);
        let c = candidates(&resp, &film_query(), Some("Heat.1995.1080p.BluRay.x264-GRP"));
        // the two-CD subtitle and the German one are left out
        assert_eq!(c.len(), 4);
        let hash = c.iter().find(|c| c.level == MatchLevel::Hash).unwrap();
        assert_eq!(hash.source_id, "7001");
        assert_eq!(hash.variant, Variant::Normal);
        assert!(
            c.iter()
                .any(|c| c.variant == Variant::Sdh && c.source_id == "7002")
        );
        assert!(
            c.iter()
                .any(|c| c.variant == Variant::Forced && c.source_id == "7003")
        );
        let pt = c.iter().find(|c| c.language == "pt").unwrap();
        assert!(pt.machine_translated);
        assert_eq!(pt.level, MatchLevel::Id);
    }

    #[test]
    fn hash_flag_without_a_hash_query_is_not_trusted() {
        let resp: SearchResponse = serde_json::from_str(SEARCH).unwrap();
        let q = Query {
            moviehash: None,
            ..film_query()
        };
        assert!(
            candidates(&resp, &q, None)
                .iter()
                .all(|c| c.level == MatchLevel::Id)
        );
    }

    #[test]
    fn name_only_searches_need_a_close_release() {
        let resp: SearchResponse = serde_json::from_str(SEARCH).unwrap();
        let q = Query {
            moviehash: None,
            imdb_id: None,
            tmdb_id: None,
            title: Some("Heat".into()),
            ..film_query()
        };
        let c = candidates(&resp, &q, Some("Heat.1995.1080p.BluRay.x264-GRP"));
        assert!(!c.is_empty());
        assert!(c.iter().all(|c| c.level == MatchLevel::Release));
        assert!(
            c.iter()
                .all(|c| c.release.as_deref().unwrap().contains("1080p.BluRay"))
        );
    }
}
