//! Spacing between requests to one source, so Reelix stays inside its rate limits.

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Lets one request through at a time, at least `gap` apart; a source's "slow down" answer
/// pushes the next request back.
#[derive(Debug)]
pub struct Pacer {
    gap: Duration,
    next: Mutex<Option<Instant>>,
}

impl Pacer {
    pub fn new(gap: Duration) -> Self {
        Self {
            gap,
            next: Mutex::new(None),
        }
    }

    /// Waits for this request's turn.
    pub async fn wait(&self) {
        let mut next = self.next.lock().await;
        if let Some(at) = *next {
            tokio::time::sleep_until(at).await;
        }
        *next = Some(Instant::now() + self.gap);
    }

    /// Holds every request back for `delay` (from a `Retry-After` answer).
    pub async fn back_off(&self, delay: Duration) {
        let mut next = self.next.lock().await;
        let at = Instant::now() + delay;
        if next.is_none_or(|n| n < at) {
            *next = Some(at);
        }
    }
}

/// Seconds from a `Retry-After` header, at most a minute; 10 seconds when it is missing.
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Duration {
    let secs = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(10);
    Duration::from_secs(secs.clamp(1, 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn requests_are_spaced() {
        let p = Pacer::new(Duration::from_secs(1));
        let start = Instant::now();
        p.wait().await;
        p.wait().await;
        p.wait().await;
        assert!(start.elapsed() >= Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn back_off_holds_the_next_request() {
        let p = Pacer::new(Duration::from_millis(10));
        let start = Instant::now();
        p.back_off(Duration::from_secs(5)).await;
        p.wait().await;
        assert!(start.elapsed() >= Duration::from_secs(5));
    }

    #[test]
    fn retry_after_is_capped() {
        let mut h = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&h), Duration::from_secs(10));
        h.insert(reqwest::header::RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(retry_after(&h), Duration::from_secs(60));
    }
}
