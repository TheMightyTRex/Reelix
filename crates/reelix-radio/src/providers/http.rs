//! A small cached, paced JSON reader shared by the providers.
//!
//! - Responses are cached in memory per URL for `ttl` (bounded to [`MAX_ENTRIES`]);
//!   [`JsonReader::get_for`] reads with another cache time (e.g. Discover lists).
//! - Requests are spaced at least `interval` apart.
//! - `429 Too Many Requests` honours `Retry-After`: a short wait (≤ [`MAX_INLINE_WAIT`]) is
//!   slept once and retried; a longer one fails with [`ProviderError::RateLimited`] and
//!   every request until then fails the same way without reaching the source.
//! - Bodies over `max_bytes` are refused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use tokio::time::Instant;

use crate::provider::ProviderError;

/// Most cached responses kept per reader.
pub const MAX_ENTRIES: usize = 512;

/// Longest `Retry-After` slept inside a request.
pub const MAX_INLINE_WAIT: Duration = Duration::from_secs(5);

pub struct JsonReader {
    http: reqwest::Client,
    /// Name used in messages ("the BBC", "Podcast Index").
    source: &'static str,
    interval: Duration,
    ttl: Duration,
    max_bytes: usize,
    /// Time of the last request; held across the wait so requests queue up.
    last: tokio::sync::Mutex<Option<Instant>>,
    cache: Mutex<HashMap<String, (Instant, Arc<serde_json::Value>)>>,
    blocked_until: Mutex<Option<Instant>>,
}

impl JsonReader {
    pub fn new(http: reqwest::Client, source: &'static str, interval: Duration, ttl: Duration) -> Self {
        Self {
            http,
            source,
            interval,
            ttl,
            max_bytes: 8 * 1024 * 1024,
            last: tokio::sync::Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            blocked_until: Mutex::new(None),
        }
    }

    /// Spacing between requests (tests use zero).
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// GETs `url` as JSON (cached). `headers` are added to the request (not part of the cache
    /// key). `404`/`410` → [`ProviderError::NotFound`].
    pub async fn get(&self, url: &str, headers: HeaderMap) -> Result<Arc<serde_json::Value>, ProviderError> {
        self.get_for(url, headers, self.ttl).await
    }

    /// [`get`](Self::get) with its own cache time `ttl` for this URL (same pacing, rate-limit
    /// handling and size limit).
    pub async fn get_for(
        &self,
        url: &str,
        headers: HeaderMap,
        ttl: Duration,
    ) -> Result<Arc<serde_json::Value>, ProviderError> {
        if let Some(v) = self.cached(url, ttl) {
            return Ok(v);
        }
        let mut retried = false;
        loop {
            self.check_blocked()?;
            self.pace().await;
            let res = self
                .http
                .get(url)
                .headers(headers.clone())
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|e| self.network(e))?;
            let status = res.status();
            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
                let wait = retry_after(res.headers());
                if !retried && let Some(w) = wait.filter(|w| *w <= MAX_INLINE_WAIT) {
                    retried = true;
                    tokio::time::sleep(w).await;
                    continue;
                }
                if status == StatusCode::SERVICE_UNAVAILABLE && wait.is_none() {
                    return Err(ProviderError::Network(format!(
                        "{} is unavailable ({status}); try again later",
                        self.source
                    )));
                }
                let wait = wait.unwrap_or(Duration::from_secs(60));
                if let Ok(mut b) = self.blocked_until.lock() {
                    *b = Some(Instant::now() + wait);
                }
                return Err(ProviderError::RateLimited {
                    retry_after: Some(wait),
                });
            }
            if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                return Err(ProviderError::NotFound(format!(
                    "{} has no such item ({status})",
                    self.source
                )));
            }
            if !status.is_success() {
                return Err(ProviderError::Network(format!(
                    "{} answered {status}",
                    self.source
                )));
            }
            let body = read_limited(res, self.max_bytes).await.map_err(|e| match e {
                ReadError::TooLarge => ProviderError::Parse(format!(
                    "{} sent more than {} MB",
                    self.source,
                    self.max_bytes / (1024 * 1024)
                )),
                ReadError::Network(e) => self.network(e),
            })?;
            let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
                ProviderError::Parse(format!("{} sent something that is not JSON ({e})", self.source))
            })?;
            let v = Arc::new(v);
            if !ttl.is_zero() {
                self.store(url, v.clone());
            }
            return Ok(v);
        }
    }

    fn network(&self, e: reqwest::Error) -> ProviderError {
        ProviderError::Network(format!("{}: {}", self.source, e.without_url()))
    }

    fn cached(&self, url: &str, ttl: Duration) -> Option<Arc<serde_json::Value>> {
        let cache = self.cache.lock().ok()?;
        let (at, v) = cache.get(url)?;
        (at.elapsed() < ttl).then(|| v.clone())
    }

    fn store(&self, url: &str, v: Arc<serde_json::Value>) {
        let Ok(mut cache) = self.cache.lock() else { return };

        if cache.len() >= MAX_ENTRIES {
            let ttl = self.ttl;
            cache.retain(|_, (at, _)| at.elapsed() < ttl);
            if cache.len() >= MAX_ENTRIES
                && let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(url.to_owned(), (Instant::now(), v));
    }

    fn check_blocked(&self) -> Result<(), ProviderError> {
        let Ok(mut b) = self.blocked_until.lock() else {
            return Ok(());
        };
        match *b {
            Some(until) if until > Instant::now() => Err(ProviderError::RateLimited {
                retry_after: Some(until - Instant::now()),
            }),
            Some(_) => {
                *b = None;
                Ok(())
            }
            None => Ok(()),
        }
    }

    async fn pace(&self) {
        let mut last = self.last.lock().await;
        if let Some(t) = *last {
            let next = t + self.interval;
            if next > Instant::now() {
                tokio::time::sleep_until(next).await;
            }
        }
        *last = Some(Instant::now());
    }
}

/// `Retry-After` in seconds (an HTTP date is read as one minute).
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    Some(match v.parse::<u64>() {
        Ok(s) => Duration::from_secs(s.min(24 * 3600)),
        Err(_) => Duration::from_secs(60),
    })
}

pub enum ReadError {
    TooLarge,
    Network(reqwest::Error),
}

/// Reads a response body, refusing more than `max` bytes.
pub async fn read_limited(mut res: reqwest::Response, max: usize) -> Result<Vec<u8>, ReadError> {
    if res.content_length().is_some_and(|n| n > max as u64) {
        return Err(ReadError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(ReadError::Network)? {
        if body.len() + chunk.len() > max {
            return Err(ReadError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

// ---------------- small JSON helpers ----------------

/// Trimmed, non-empty string at `key`.
pub fn str_at<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str().map(str::trim).filter(|s| !s.is_empty())
}

/// Positive whole number at `key` (a number or a numeric string).
pub fn u32_at(v: &serde_json::Value, key: &str) -> Option<u32> {
    let x = v.get(key)?;
    let n = x
        .as_u64()
        .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))?;
    u32::try_from(n).ok().filter(|n| *n > 0)
}

/// An http(s) URL at `key`.
pub fn url_at(v: &serde_json::Value, key: &str) -> Option<String> {
    str_at(v, key)
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_values() {
        let mut h = HeaderMap::new();
        assert_eq!(retry_after(&h), None);
        h.insert(RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_secs(7)));
        h.insert(RETRY_AFTER, "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_secs(60)));
    }

    #[test]
    fn json_helpers() {
        let v = serde_json::json!({"a": " x ", "b": "", "n": 3, "s": "4", "z": 0, "u": "https://e/x", "f": "ftp://e"});
        assert_eq!(str_at(&v, "a"), Some("x"));
        assert_eq!(str_at(&v, "b"), None);
        assert_eq!(u32_at(&v, "n"), Some(3));
        assert_eq!(u32_at(&v, "s"), Some(4));
        assert_eq!(u32_at(&v, "z"), None);
        assert_eq!(url_at(&v, "u").as_deref(), Some("https://e/x"));
        assert_eq!(url_at(&v, "f"), None);
    }
}
