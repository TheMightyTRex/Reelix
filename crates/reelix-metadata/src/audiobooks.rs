//! Audiobook details (RFX-86): narrators, length, abridgement, recording date and region from
//! public catalogues, for books Open Library knows little about as recordings.
//!
//! Sources, in order:
//! - **Audible catalogue** (`api.audible.{tld}/1.0/catalog/products`): public, no key or
//!   sign-in. One region (marketplace) at a time.
//! - **Audnexus** (`api.audnex.us/books/{asin}`): community API; only fills fields Audible left
//!   empty, and only for the same ASIN, title and authors.
//! - **Libro.fm** (`libro.fm/api/v12/explore/search`): its public search, used only when Audible
//!   has no match and the source is activated. Off by default: Libro.fm publishes no API terms.
//!   Pages are never read.
//!
//! Every source sends an honest user agent, is paced (at most one request a second each) and
//! retried only on 429 and 5xx. Answers are kept for a day in memory; the books service keeps the
//! chosen result for 30 days.

use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::MetaError;
use crate::books::SeriesRef;
use crate::http::{RateLimiter, RetryPolicy, TtlCache, send};

pub const AUDIBLE_RESPONSE_GROUPS: &str = "contributors,product_attrs,product_desc,series,media";
pub const DEFAULT_AUDNEXUS_URL: &str = "https://api.audnex.us";
pub const DEFAULT_LIBROFM_URL: &str = "https://libro.fm";
/// Search results read from one catalogue.
const SEARCH_LIMIT: usize = 10;
/// Most names kept per list (authors, narrators), so a malformed answer stays small.
const MAX_NAMES: usize = 20;

/// Audible marketplaces, by the short code people know them by.
pub const REGIONS: &[(&str, &str)] = &[
    ("us", "com"),
    ("uk", "co.uk"),
    ("au", "com.au"),
    ("ca", "ca"),
    ("de", "de"),
    ("es", "es"),
    ("fr", "fr"),
    ("in", "in"),
    ("it", "it"),
    ("jp", "co.jp"),
];

/// The Audible domain ending of `region` (`uk` → `co.uk`), if it is a known marketplace.
pub fn region_tld(region: &str) -> Option<&'static str> {
    let r = region.trim().to_ascii_lowercase();
    REGIONS.iter().find(|(code, _)| *code == r).map(|(_, tld)| *tld)
}

/// One audiobook recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AudiobookDetails {
    pub asin: Option<String>,
    pub isbn: Option<String>,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub narrators: Vec<String>,
    pub runtime_minutes: Option<u32>,
    /// `Some(true)` abridged, `Some(false)` unabridged, `None` not known.
    pub abridged: Option<bool>,
    /// Recording release date, `YYYY-MM-DD`.
    pub release_date: Option<String>,
    pub language: Option<String>,
    pub publisher: Option<String>,
    pub series: Vec<SeriesRef>,
    pub cover_url: Option<String>,
    /// Marketplace the details came from (`uk`), when the source has regions.
    pub region: Option<String>,
    /// Sources that gave any field, in order (`audible`, `audnexus`, `librofm`).
    pub sources: Vec<String>,
    /// The recording's public page.
    pub url: Option<String>,
}

/// Which sources to use and where they are.
#[derive(Debug, Clone)]
pub struct AudiobookSourcesOptions {
    pub audible: bool,
    pub audnexus: bool,
    pub librofm: bool,
    /// Audible marketplace (`us`, `uk`…); unknown codes fall back to `us`.
    pub region: String,
    /// Overrides `https://api.audible.{tld}` (tests, a caching proxy).
    pub audible_url: Option<String>,
    pub audnexus_url: String,
    pub librofm_url: String,
    pub retry: RetryPolicy,
    pub cache_ttl: Duration,
    /// Requests per second to each source.
    pub per_second: usize,
}

impl Default for AudiobookSourcesOptions {
    fn default() -> Self {
        Self {
            audible: true,
            audnexus: true,
            librofm: false,
            region: "us".into(),
            audible_url: None,
            audnexus_url: DEFAULT_AUDNEXUS_URL.into(),
            librofm_url: DEFAULT_LIBROFM_URL.into(),
            retry: RetryPolicy {
                max_retries: 2,
                ..RetryPolicy::default()
            },
            cache_ttl: Duration::from_secs(24 * 3600),
            per_second: 1,
        }
    }
}

/// What to look up: a known ASIN is used first, then title and author.
#[derive(Debug, Clone, Default)]
pub struct AudiobookQuery {
    pub title: String,
    pub authors: Vec<String>,
    pub asin: Option<String>,
    pub isbn: Option<String>,
}

/// Audible, Audnexus and Libro.fm behind one lookup. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AudiobookSources {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    opts: AudiobookSourcesOptions,
    user_agent: String,
    cache: TtlCache,
    audible: RateLimiter,
    audnexus: RateLimiter,
    librofm: RateLimiter,
}

impl AudiobookSources {
    pub fn new(http: reqwest::Client, opts: AudiobookSourcesOptions) -> Self {
        let per = Duration::from_secs(1);
        let n = opts.per_second.max(1);
        Self {
            inner: Arc::new(Inner {
                http,
                user_agent: format!(
                    "Reelix/{} ( https://github.com/demonzoned/Reelix )",
                    reelix_core::VERSION
                ),
                cache: TtlCache::new(200).with_max_bytes(4 << 20),
                audible: RateLimiter::new(n, per),
                audnexus: RateLimiter::new(n, per),
                librofm: RateLimiter::new(n, per),
                opts,
            }),
        }
    }

    pub fn options(&self) -> &AudiobookSourcesOptions {
        &self.inner.opts
    }

    fn region(&self) -> &'static str {
        let r = self.inner.opts.region.trim().to_ascii_lowercase();
        REGIONS
            .iter()
            .find(|(code, _)| *code == r)
            .map_or("us", |(code, _)| code)
    }

    fn audible_base(&self) -> String {
        match &self.inner.opts.audible_url {
            Some(u) => u.trim_end_matches('/').to_string(),
            None => format!(
                "https://api.audible.{}",
                region_tld(self.region()).unwrap_or("com")
            ),
        }
    }

    /// The best recording for `q`, with Audnexus filling gaps, or `None` when no activated
    /// source has a match. An error is returned only when every source asked failed.
    pub async fn lookup(&self, q: &AudiobookQuery) -> Result<Option<AudiobookDetails>, MetaError> {
        let o = &self.inner.opts;
        let mut last_err = None;
        let mut found = None;
        if o.audible {
            match self.audible_lookup(q).await {
                Ok(d) => found = d,
                Err(e) => {
                    tracing::debug!(error = %e, "audible lookup failed");
                    last_err = Some(e);
                }
            }
        }
        if let Some(d) = found.as_mut()
            && o.audnexus
            && (d.narrators.is_empty()
                || d.runtime_minutes.is_none()
                || d.release_date.is_none()
                || d.publisher.is_none())
            && let Some(asin) = d.asin.clone()
        {
            match self.audnexus(&asin).await {
                Ok(other) => fill_from(d, &other),
                Err(e) => tracing::debug!(error = %e, "audnexus lookup failed"),
            }
        }
        if found.is_none() && !o.audible && o.audnexus && q.asin.as_deref().is_some_and(valid_asin) {
            // Audible deactivated: Audnexus alone for a known ASIN.
            match self.audnexus(q.asin.as_deref().unwrap_or_default()).await {
                Ok(d) => found = Some(d),
                Err(MetaError::NotFound) => {}
                Err(e) => last_err = Some(e),
            }
        }
        if found.is_none() && o.librofm {
            match self.librofm_search(q).await {
                Ok(hits) => found = best_match(q, hits),
                Err(e) => {
                    tracing::debug!(error = %e, "libro.fm lookup failed");
                    last_err = Some(e);
                }
            }
        }
        match (found, last_err) {
            (Some(d), _) => Ok(Some(d)),
            (None, Some(e)) if !matches!(e, MetaError::NotFound) => Err(e),
            _ => Ok(None),
        }
    }

    async fn audible_lookup(&self, q: &AudiobookQuery) -> Result<Option<AudiobookDetails>, MetaError> {
        if let Some(asin) = q.asin.as_deref().filter(|a| valid_asin(a)) {
            match self.audible_product(asin).await {
                Ok(d) if matches_query(q, &d) => return Ok(Some(d)),
                Ok(_) | Err(MetaError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        if q.title.trim().is_empty() {
            return Ok(None);
        }
        Ok(best_match(q, self.audible_search(q).await?))
    }

    /// One Audible product by ASIN.
    pub async fn audible_product(&self, asin: &str) -> Result<AudiobookDetails, MetaError> {
        let asin = checked_asin(asin)?;
        let url = format!("{}/1.0/catalog/products/{asin}", self.audible_base());
        let v = self
            .get(
                &url,
                &[("response_groups", AUDIBLE_RESPONSE_GROUPS.into())],
                &self.inner.audible,
            )
            .await?;
        let d = audible_product(v.get("product").unwrap_or(&Value::Null), self.region())
            .ok_or(MetaError::NotFound)?;
        if d.asin.as_deref() != Some(asin.as_str()) {
            return Err(MetaError::Decode(
                "the catalogue returned a different recording".into(),
            ));
        }
        Ok(d)
    }

    /// Audible catalogue search by title and author.
    pub async fn audible_search(&self, q: &AudiobookQuery) -> Result<Vec<AudiobookDetails>, MetaError> {
        let url = format!("{}/1.0/catalog/products", self.audible_base());
        let mut query = vec![
            ("title", q.title.trim().to_string()),
            ("num_results", SEARCH_LIMIT.to_string()),
            ("products_sort_by", "Relevance".into()),
            ("response_groups", AUDIBLE_RESPONSE_GROUPS.into()),
        ];
        if let Some(a) = q.authors.first().filter(|a| !a.trim().is_empty()) {
            query.push(("author", a.trim().to_string()));
        }
        let v = self.get(&url, &query, &self.inner.audible).await?;
        let rows = v
            .get("products")
            .and_then(Value::as_array)
            .ok_or_else(|| MetaError::Decode("the audiobook search answer has no products".into()))?;
        let region = self.region();
        Ok(rows
            .iter()
            .take(SEARCH_LIMIT)
            .filter_map(|r| audible_product(r, region))
            .collect())
    }

    /// Audnexus details of one ASIN in the configured region.
    pub async fn audnexus(&self, asin: &str) -> Result<AudiobookDetails, MetaError> {
        let asin = checked_asin(asin)?;
        let url = format!(
            "{}/books/{asin}",
            self.inner.opts.audnexus_url.trim_end_matches('/')
        );
        let region = self.region();
        let v = self
            .get(&url, &[("region", region.to_string())], &self.inner.audnexus)
            .await?;
        audnexus_book(&v, region).ok_or(MetaError::NotFound)
    }

    /// Libro.fm's public search (no pages are read).
    pub async fn librofm_search(&self, q: &AudiobookQuery) -> Result<Vec<AudiobookDetails>, MetaError> {
        let url = format!(
            "{}/api/v12/explore/search",
            self.inner.opts.librofm_url.trim_end_matches('/')
        );
        let (term, by) = match q.isbn.as_deref().filter(|i| valid_isbn(i)) {
            Some(isbn) => (isbn.to_string(), "isbn"),
            None => {
                let author = q.authors.first().map(String::as_str).unwrap_or_default();
                (format!("{} {author}", q.title.trim()).trim().to_string(), "all")
            }
        };
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let v = self
            .get(
                &url,
                &[("page", "1".into()), ("q", term), ("searchby", by.into())],
                &self.inner.librofm,
            )
            .await?;
        librofm_hits(&v)
    }

    async fn get(
        &self,
        url: &str,
        query: &[(&str, String)],
        limiter: &RateLimiter,
    ) -> Result<Arc<Value>, MetaError> {
        let inner = &self.inner;
        let key = format!("{url}?{query:?}");
        if let Some(v) = inner.cache.get(&key) {
            return Ok(v);
        }
        let resp = send(
            || {
                inner
                    .http
                    .get(url)
                    .query(query)
                    .timeout(Duration::from_secs(20))
                    .header(reqwest::header::USER_AGENT, &inner.user_agent)
                    .header(reqwest::header::ACCEPT, "application/json")
            },
            Some(limiter),
            &inner.opts.retry,
        )
        .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if !status.is_success() {
            return Err(match status {
                StatusCode::NOT_FOUND => MetaError::NotFound,
                StatusCode::TOO_MANY_REQUESTS => MetaError::RateLimited {
                    retry_after_secs: None,
                },
                _ => MetaError::Status {
                    status: status.as_u16(),
                    message: String::from_utf8_lossy(&body).chars().take(200).collect(),
                },
            });
        }
        if body.len() > 4 << 20 {
            return Err(MetaError::Decode("the answer is larger than 4 MB".into()));
        }
        let v = Arc::new(serde_json::from_slice::<Value>(&body)?);
        inner.cache.insert(key, v.clone(), inner.opts.cache_ttl);
        Ok(v)
    }
}

/// Fills empty fields of `d` from `other` when both describe the same recording.
pub fn fill_from(d: &mut AudiobookDetails, other: &AudiobookDetails) {
    if d.asin != other.asin
        || clean(&d.title) != clean(&other.title)
        || !authors_overlap(&d.authors, &other.authors)
    {
        return;
    }
    let before = (
        d.narrators.len(),
        d.runtime_minutes,
        d.release_date.clone(),
        d.publisher.clone(),
    );
    if d.narrators.is_empty() {
        d.narrators.clone_from(&other.narrators);
    }
    d.runtime_minutes = d.runtime_minutes.or(other.runtime_minutes);
    d.abridged = d.abridged.or(other.abridged);
    if d.release_date.is_none() {
        d.release_date.clone_from(&other.release_date);
    }
    if d.publisher.is_none() {
        d.publisher.clone_from(&other.publisher);
    }
    if d.isbn.is_none() {
        d.isbn.clone_from(&other.isbn);
    }
    if d.language.is_none() {
        d.language.clone_from(&other.language);
    }
    if d.series.is_empty() {
        d.series.clone_from(&other.series);
    }
    if d.cover_url.is_none() {
        d.cover_url.clone_from(&other.cover_url);
    }
    let after = (
        d.narrators.len(),
        d.runtime_minutes,
        d.release_date.clone(),
        d.publisher.clone(),
    );
    if before != after {
        for s in &other.sources {
            if !d.sources.contains(s) {
                d.sources.push(s.clone());
            }
        }
    }
}

/// The first hit whose title and authors match the query, preferring unabridged recordings.
pub fn best_match(q: &AudiobookQuery, hits: Vec<AudiobookDetails>) -> Option<AudiobookDetails> {
    let mut matching: Vec<AudiobookDetails> = hits.into_iter().filter(|h| matches_query(q, h)).collect();
    let pos = matching
        .iter()
        .position(|h| h.abridged == Some(false))
        .or_else(|| matching.iter().position(|h| h.abridged.is_none()))
        .unwrap_or(0);
    (!matching.is_empty()).then(|| matching.swap_remove(pos))
}

/// Same book: an equal ASIN, or the titles agree (ignoring subtitles, articles and
/// punctuation) and, when the query names authors, at least one surname is shared.
pub fn matches_query(q: &AudiobookQuery, d: &AudiobookDetails) -> bool {
    if let (Some(a), Some(b)) = (q.asin.as_deref(), d.asin.as_deref())
        && a.eq_ignore_ascii_case(b)
    {
        return true;
    }
    if let (Some(a), Some(b)) = (q.isbn.as_deref(), d.isbn.as_deref())
        && digits(a) == digits(b)
        && !digits(a).is_empty()
    {
        return true;
    }
    let (qt, dt) = (clean(&q.title), clean(&d.title));
    if qt.is_empty() || dt.is_empty() {
        return false;
    }
    let titles = qt == dt || main_title(&q.title) == main_title(&d.title) || qt == main_title(&d.title);
    titles && (q.authors.is_empty() || authors_overlap(&q.authors, &d.authors))
}

fn authors_overlap(a: &[String], b: &[String]) -> bool {
    let surnames = |v: &[String]| -> Vec<String> {
        v.iter()
            .filter_map(|n| clean(n).split(' ').next_back().map(str::to_string))
            .filter(|s| !s.is_empty())
            .collect()
    };
    let (a, b) = (surnames(a), surnames(b));
    a.iter().any(|x| b.contains(x))
}

/// Lower case letters and digits, single spaces, no leading article.
pub fn clean(s: &str) -> String {
    let folded: String = s
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = folded.split_whitespace().collect();
    let words = match words.first() {
        Some(&("the" | "a" | "an")) if words.len() > 1 => &words[1..],
        _ => &words[..],
    };
    words.join(" ")
}

/// The title before a subtitle or a bracket (`"Dune: Deluxe Edition"` → `"dune"`).
fn main_title(s: &str) -> String {
    let cut = s.find([':', '(', '[']).unwrap_or(s.len());
    clean(&s[..cut])
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

pub fn valid_asin(s: &str) -> bool {
    let s = s.trim();
    s.len() == 10 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn valid_isbn(s: &str) -> bool {
    matches!(digits(s).len(), 10 | 13)
}

fn checked_asin(s: &str) -> Result<String, MetaError> {
    if valid_asin(s) {
        Ok(s.trim().to_ascii_uppercase())
    } else {
        Err(MetaError::InvalidSettings(
            "an ASIN is 10 letters and digits".into(),
        ))
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(|s| strip_tags(s).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(4000));
    let mut in_tag = false;
    for c in s.chars().take(20_000) {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn names(v: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in v.and_then(Value::as_array).into_iter().flatten() {
        let name = n
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| n.as_str())
            .map(str::trim)
            .unwrap_or_default();
        if !name.is_empty() && !out.iter().any(|o| o == name) && out.len() < MAX_NAMES {
            out.push(name.to_string());
        }
    }
    out
}

fn minutes(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|m| *m > 0)
}

fn abridged(v: Option<String>) -> Option<bool> {
    match v?.to_ascii_lowercase().as_str() {
        "abridged" => Some(true),
        "unabridged" => Some(false),
        _ => None,
    }
}

fn day(v: Option<String>) -> Option<String> {
    let s = v?;
    let d = s.get(..10)?;
    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
        .ok()
        .map(|_| d.to_string())
}

fn language(v: Option<String>) -> Option<String> {
    v.map(|l| {
        let mut c = l.chars();
        match c.next() {
            Some(f) => f.to_uppercase().chain(c).collect(),
            None => l,
        }
    })
}

/// An Audible catalogue product.
pub fn audible_product(v: &Value, region: &str) -> Option<AudiobookDetails> {
    let asin = text(v, "asin").filter(|a| valid_asin(a))?;
    let title = text(v, "title")?;
    let images = v.get("product_images");
    let cover = ["500", "1024", "300"]
        .iter()
        .find_map(|k| images.and_then(|i| i.get(*k)).and_then(Value::as_str))
        .map(str::to_string);
    let series = v
        .get("series")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(SeriesRef {
                title: text(s, "title")?,
                position: text(s, "sequence"),
            })
        })
        .collect();
    let url = region_tld(region).map(|tld| format!("https://www.audible.{tld}/pd/{asin}"));
    Some(AudiobookDetails {
        asin: Some(asin),
        isbn: text(v, "isbn"),
        title,
        subtitle: text(v, "subtitle"),
        authors: names(v.get("authors")),
        narrators: names(v.get("narrators")),
        runtime_minutes: minutes(v.get("runtime_length_min")),
        abridged: abridged(text(v, "format_type")),
        release_date: day(text(v, "release_date").or_else(|| text(v, "issue_date"))),
        language: language(text(v, "language")),
        publisher: text(v, "publisher_name"),
        series,
        cover_url: cover,
        region: Some(region.to_string()),
        sources: vec!["audible".into()],
        url,
    })
}

/// An Audnexus book.
pub fn audnexus_book(v: &Value, region: &str) -> Option<AudiobookDetails> {
    let asin = text(v, "asin").filter(|a| valid_asin(a))?;
    let title = text(v, "title")?;
    let series = ["seriesPrimary", "seriesSecondary"]
        .iter()
        .filter_map(|k| v.get(*k))
        .filter_map(|s| {
            Some(SeriesRef {
                title: text(s, "name").or_else(|| text(s, "title"))?,
                position: text(s, "position").or_else(|| text(s, "sequence")),
            })
        })
        .collect();
    Some(AudiobookDetails {
        asin: Some(asin),
        isbn: text(v, "isbn"),
        title,
        subtitle: text(v, "subtitle"),
        authors: names(v.get("authors")),
        narrators: names(v.get("narrators")),
        runtime_minutes: minutes(v.get("runtimeLengthMin")),
        abridged: abridged(text(v, "formatType")),
        release_date: day(text(v, "releaseDate")),
        language: language(text(v, "language")),
        publisher: text(v, "publisherName"),
        series,
        cover_url: text(v, "image"),
        region: text(v, "region").or_else(|| Some(region.to_string())),
        sources: vec!["audnexus".into()],
        url: None,
    })
}

/// Libro.fm search hits. A changed answer shape is an error, not an empty list.
pub fn librofm_hits(v: &Value) -> Result<Vec<AudiobookDetails>, MetaError> {
    let rows = v
        .get("audiobook_collection")
        .and_then(|c| c.get("audiobooks"))
        .and_then(Value::as_array)
        .ok_or_else(|| MetaError::Decode("the Libro.fm search answer changed shape".into()))?;
    Ok(rows
        .iter()
        .take(SEARCH_LIMIT * 2)
        .filter_map(|r| {
            let title = text(r, "title")?;
            let isbn = match r.get("isbn")? {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.trim().to_string(),
                _ => return None,
            };
            if !valid_isbn(&isbn) {
                return None;
            }
            let cover = text(r, "cover_url").map(|c| {
                if c.starts_with("//") {
                    format!("https:{c}")
                } else {
                    c
                }
            });
            Some(AudiobookDetails {
                url: Some(format!("https://libro.fm/audiobooks/{isbn}")),
                isbn: Some(isbn),
                title,
                authors: names(r.get("authors")),
                narrators: names(r.get("audiobook_info").and_then(|i| i.get("narrators"))),
                cover_url: cover,
                sources: vec!["librofm".into()],
                ..AudiobookDetails::default()
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn q(title: &str, author: &str) -> AudiobookQuery {
        AudiobookQuery {
            title: title.into(),
            authors: vec![author.into()],
            ..AudiobookQuery::default()
        }
    }

    fn hit(title: &str, author: &str, abridged: Option<bool>) -> AudiobookDetails {
        AudiobookDetails {
            title: title.into(),
            authors: vec![author.into()],
            abridged,
            ..AudiobookDetails::default()
        }
    }

    #[test]
    fn titles_and_authors_match_loosely() {
        assert!(matches_query(
            &q("The Hobbit", "J. R. R. Tolkien"),
            &hit("Hobbit: Deluxe Edition", "J.R.R. Tolkien", None)
        ));
        assert!(matches_query(
            &q("Dune", "Frank Herbert"),
            &hit("Dune", "Frank Herbert", None)
        ));
        assert!(!matches_query(
            &q("Dune", "Frank Herbert"),
            &hit("Dune Messiah", "Frank Herbert", None)
        ));
        assert!(!matches_query(
            &q("Dune", "Frank Herbert"),
            &hit("Dune", "Brian Herbertson", None)
        ));
    }

    #[test]
    fn unabridged_is_preferred() {
        let best = best_match(
            &q("Dune", "Frank Herbert"),
            vec![
                hit("Dune", "Frank Herbert", Some(true)),
                hit("Dune", "Frank Herbert", Some(false)),
            ],
        )
        .unwrap();
        assert_eq!(best.abridged, Some(false));
        assert!(
            best_match(
                &q("Dune", "Frank Herbert"),
                vec![hit("Emma", "Jane Austen", None)]
            )
            .is_none()
        );
    }

    #[test]
    fn reads_an_audible_product() {
        let v = json!({
            "asin": "B08G9PRS1K", "title": "Project Hail Mary",
            "authors": [{"asin": "B00G0WYW92", "name": "Andy Weir"}],
            "narrators": [{"name": "Ray Porter"}, {"name": "Ray Porter"}],
            "runtime_length_min": 970, "format_type": "unabridged",
            "release_date": "2021-05-04", "language": "english",
            "publisher_name": "Audible Studios",
            "series": [{"asin": "X", "title": "Hail Mary", "sequence": "1"}],
            "product_images": {"500": "https://m.media-amazon.com/images/I/a.jpg"},
            "merchandising_summary": "<p>Alone</p>"
        });
        let d = audible_product(&v, "uk").unwrap();
        assert_eq!(d.narrators, vec!["Ray Porter".to_string()]);
        assert_eq!(d.runtime_minutes, Some(970));
        assert_eq!(d.abridged, Some(false));
        assert_eq!(d.release_date.as_deref(), Some("2021-05-04"));
        assert_eq!(d.language.as_deref(), Some("English"));
        assert_eq!(d.series[0].position.as_deref(), Some("1"));
        assert_eq!(d.url.as_deref(), Some("https://www.audible.co.uk/pd/B08G9PRS1K"));
        assert!(audible_product(&json!({"asin": "bad", "title": "x"}), "us").is_none());
    }

    #[test]
    fn reads_an_audnexus_book_and_fills_gaps() {
        let v = json!({
            "asin": "B08G9PRS1K", "title": "Project Hail Mary",
            "authors": [{"name": "Andy Weir"}], "narrators": [{"name": "Ray Porter"}],
            "runtimeLengthMin": 970, "formatType": "unabridged",
            "releaseDate": "2021-05-04T00:00:00.000Z", "publisherName": "Audible Studios",
            "seriesPrimary": {"asin": "Y", "name": "Hail Mary", "position": "1"}, "region": "us"
        });
        let nexus = audnexus_book(&v, "us").unwrap();
        assert_eq!(nexus.release_date.as_deref(), Some("2021-05-04"));
        assert_eq!(nexus.series[0].title, "Hail Mary");
        let mut d = AudiobookDetails {
            asin: Some("B08G9PRS1K".into()),
            title: "Project Hail Mary".into(),
            authors: vec!["Andy Weir".into()],
            sources: vec!["audible".into()],
            ..AudiobookDetails::default()
        };
        fill_from(&mut d, &nexus);
        assert_eq!(d.narrators, vec!["Ray Porter".to_string()]);
        assert_eq!(d.runtime_minutes, Some(970));
        assert_eq!(d.sources, vec!["audible".to_string(), "audnexus".to_string()]);
        // A different recording never fills.
        let mut other = d.clone();
        other.narrators.clear();
        other.asin = Some("B000000000".into());
        fill_from(&mut other, &nexus);
        assert!(other.narrators.is_empty());
    }

    #[test]
    fn reads_librofm_hits() {
        let v = json!({"audiobook_collection": {"audiobooks": [
            {"title": "Project Hail Mary", "isbn": 9781603935470_u64, "authors": ["Andy Weir"],
             "cover_url": "//covers.libro.fm/9781603935470_1120.jpg",
             "catalog_info": {"coming_soon": false}, "audiobook_info": {"narrators": ["Ray Porter"]}},
            {"title": "Broken", "isbn": "12"}
        ]}});
        let hits = librofm_hits(&v).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].isbn.as_deref(), Some("9781603935470"));
        assert_eq!(
            hits[0].cover_url.as_deref(),
            Some("https://covers.libro.fm/9781603935470_1120.jpg")
        );
        assert!(librofm_hits(&json!({"other": 1})).is_err());
    }

    #[test]
    fn regions() {
        assert_eq!(region_tld("UK"), Some("co.uk"));
        assert_eq!(region_tld("zz"), None);
        assert_eq!(clean("The  Hobbit!"), "hobbit");
        assert!(valid_asin("B08G9PRS1K"));
        assert!(!valid_asin("B08G9"));
    }
}
