//! Typed indexer errors.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// Everything that can go wrong talking to an indexer.
#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error("invalid settings: {0}")]
    Config(String),
    #[error("unknown indexer implementation `{0}`")]
    UnknownImplementation(String),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("indexer returned HTTP {status}")]
    Status { status: u16 },
    /// Bad API key, wrong credentials, suspended account or login rejected.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The indexer refused the request because a query/grab quota was hit.
    #[error("indexer request limit reached{}", retry_after.map(|d| format!(" (retry after {}s)", d.as_secs())).unwrap_or_default())]
    RequestLimit { retry_after: Option<Duration> },
    /// Any other error reported by the indexer API (e.g. Newznab error codes).
    #[error("indexer error {code}: {description}")]
    Api { code: u32, description: String },
    #[error("could not parse indexer response: {0}")]
    Parse(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("request timed out")]
    Timeout,
    /// The manager is holding the indexer back after repeated failures.
    #[error("indexer disabled until {until}")]
    Disabled { until: DateTime<Utc> },
    /// Local rate limit would be exceeded within the allowed wait.
    #[error("rate limited locally, next slot in {}ms", wait.as_millis())]
    RateLimited { wait: Duration },
    /// Target address rejected by the SSRF guard.
    #[error("blocked address: {0}")]
    Blocked(String),
    /// A Cloudflare challenge was hit and could not be solved.
    #[error("Cloudflare challenge: {0}")]
    Cloudflare(String),
    /// A download URL redirected to a magnet link instead of returning a file.
    #[error("download redirected to magnet link")]
    MagnetRedirect(String),
    #[error("indexer `{0}` is not registered")]
    NotFound(i64),
}

impl IndexerError {
    /// Whether this error should count against the indexer's health (and trigger back-off).
    pub fn is_failure(&self) -> bool {
        !matches!(
            self,
            Self::RateLimited { .. }
                | Self::Disabled { .. }
                | Self::Unsupported(_)
                | Self::MagnetRedirect(_)
                | Self::NotFound(_)
        )
    }
}
