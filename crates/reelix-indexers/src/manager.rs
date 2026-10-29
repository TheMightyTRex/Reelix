//! Registry of live indexer instances with rate limiting, failure back-off and stats.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::{Indexer, IndexerError, ReleaseInfo, SearchQuery};

/// Escalating disable periods after consecutive failures (1st → 5 min … 8th+ → 24 h).
pub const BACKOFF: [Duration; 8] = [
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(3600),
    Duration::from_secs(3 * 3600),
    Duration::from_secs(6 * 3600),
    Duration::from_secs(12 * 3600),
    Duration::from_secs(24 * 3600),
];

/// Back-off for the n-th consecutive failure (1-based).
pub fn backoff_for(failures: u32) -> Duration {
    BACKOFF[(failures.max(1) as usize - 1).min(BACKOFF.len() - 1)]
}

/// Per-indexer request limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Max queries per `period` (None = unlimited).
    pub query_limit: Option<u32>,
    pub period: Duration,
    /// Minimum spacing between two queries.
    pub min_interval: Option<Duration>,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            query_limit: None,
            period: Duration::from_secs(3600),
            min_interval: None,
        }
    }
}

/// Persistable health state (`indexer_status` table).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexerStatus {
    pub failure_count: u32,
    pub disabled_until: Option<DateTime<Utc>>,
    pub last_failure_reason: Option<String>,
}

/// Counters snapshot for one indexer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexerStats {
    pub indexer_id: i64,
    pub name: String,
    pub queries: u64,
    pub rss_queries: u64,
    pub grabs: u64,
    pub failures: u64,
    pub avg_response_ms: u64,
    pub status: IndexerStatus,
}

/// Outcome of one indexer in a fan-out search.
#[derive(Debug)]
pub struct IndexerSearchResult {
    pub indexer_id: i64,
    pub name: String,
    pub result: Result<Vec<ReleaseInfo>, IndexerError>,
    pub elapsed: Duration,
}

#[derive(Default)]
struct Counters {
    queries: AtomicU64,
    rss: AtomicU64,
    grabs: AtomicU64,
    failures: AtomicU64,
    responses: AtomicU64,
    response_ms: AtomicU64,
}

#[derive(Default)]
struct Window {
    recent: VecDeque<Instant>,
    last: Option<Instant>,
}

struct Entry {
    name: String,
    indexer: Arc<dyn Indexer>,
    limit: RateLimit,
    window: Mutex<Window>,
    status: Mutex<IndexerStatus>,
    counters: Counters,
}

type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Holds every enabled indexer and coordinates access to them.
pub struct IndexerManager {
    entries: RwLock<HashMap<i64, Arc<Entry>>>,
    clock: Clock,
}

impl Default for IndexerManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Search,
    Rss,
    Test,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl IndexerManager {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(Utc::now))
    }

    /// Uses a custom wall clock for disable/re-enable decisions (tests).
    pub fn with_clock(clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            clock,
        }
    }

    /// Adds or replaces an instance. Health state survives a replace (settings edit).
    pub fn register(&self, id: i64, name: &str, indexer: Arc<dyn Indexer>, limit: RateLimit) {
        let mut map = self.entries.write().unwrap_or_else(|p| p.into_inner());
        let status = map.get(&id).map(|e| lock(&e.status).clone()).unwrap_or_default();
        map.insert(
            id,
            Arc::new(Entry {
                name: name.into(),
                indexer,
                limit,
                window: Mutex::new(Window::default()),
                status: Mutex::new(status),
                counters: Counters::default(),
            }),
        );
    }

    pub fn unregister(&self, id: i64) -> bool {
        self.entries
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id)
            .is_some()
    }

    fn entry(&self, id: i64) -> Option<Arc<Entry>> {
        self.entries
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .cloned()
    }

    pub fn get(&self, id: i64) -> Option<Arc<dyn Indexer>> {
        self.entry(id).map(|e| e.indexer.clone())
    }

    pub fn ids(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self
            .entries
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect();
        v.sort_unstable();
        v
    }

    /// Restores persisted health state at startup.
    pub fn restore_status(&self, id: i64, status: IndexerStatus) {
        if let Some(e) = self.entry(id) {
            *lock(&e.status) = status;
        }
    }

    pub fn status(&self, id: i64) -> Option<IndexerStatus> {
        self.entry(id).map(|e| lock(&e.status).clone())
    }

    /// True unless the indexer is in a back-off period. Expired periods re-enable automatically.
    pub fn is_available(&self, id: i64) -> bool {
        self.entry(id).is_some_and(|e| self.disabled_until(&e).is_none())
    }

    /// Clears the back-off immediately (manual re-enable / successful health check).
    pub fn reenable(&self, id: i64) {
        if let Some(e) = self.entry(id) {
            *lock(&e.status) = IndexerStatus::default();
        }
    }

    pub fn stats(&self) -> Vec<IndexerStats> {
        let map = self.entries.read().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<IndexerStats> = map
            .iter()
            .map(|(&id, e)| {
                let c = &e.counters;
                let n = c.responses.load(Ordering::Relaxed);
                IndexerStats {
                    indexer_id: id,
                    name: e.name.clone(),
                    queries: c.queries.load(Ordering::Relaxed),
                    rss_queries: c.rss.load(Ordering::Relaxed),
                    grabs: c.grabs.load(Ordering::Relaxed),
                    failures: c.failures.load(Ordering::Relaxed),
                    avg_response_ms: c.response_ms.load(Ordering::Relaxed).checked_div(n).unwrap_or(0),
                    status: lock(&e.status).clone(),
                }
            })
            .collect();
        v.sort_by_key(|s| s.indexer_id);
        v
    }

    fn disabled_until(&self, e: &Entry) -> Option<DateTime<Utc>> {
        lock(&e.status).disabled_until.filter(|&u| u > (self.clock)())
    }

    fn record(&self, e: &Entry, res: Result<(), &IndexerError>, elapsed: Duration) {
        e.counters.responses.fetch_add(1, Ordering::Relaxed);
        e.counters
            .response_ms
            .fetch_add(elapsed.as_millis() as u64, Ordering::Relaxed);
        let mut st = lock(&e.status);
        match res {
            Ok(()) => *st = IndexerStatus::default(),
            Err(err) if err.is_failure() => {
                e.counters.failures.fetch_add(1, Ordering::Relaxed);
                st.failure_count += 1;
                let mut wait = backoff_for(st.failure_count);
                if let IndexerError::RequestLimit { retry_after: Some(r) } = err {
                    wait = *r;
                }
                st.disabled_until =
                    Some((self.clock)() + chrono::Duration::from_std(wait).unwrap_or_default());
                st.last_failure_reason = Some(err.to_string());
                tracing::warn!(indexer = %e.name, failures = st.failure_count, error = %err, "indexer failed; backing off");
            }
            Err(_) => {}
        }
    }

    /// Waits for a rate-limit slot, failing if none frees up before `deadline`.
    async fn acquire(e: &Entry, deadline: Instant) -> Result<(), IndexerError> {
        loop {
            let wait = {
                let mut w = lock(&e.window);
                let now = Instant::now();
                while w
                    .recent
                    .front()
                    .is_some_and(|&t| now.duration_since(t) >= e.limit.period)
                {
                    w.recent.pop_front();
                }
                let mut wait = Duration::ZERO;
                if let (Some(min), Some(last)) = (e.limit.min_interval, w.last) {
                    wait = wait.max(min.saturating_sub(now.duration_since(last)));
                }
                if let (Some(limit), Some(&oldest)) = (e.limit.query_limit, w.recent.front())
                    && w.recent.len() >= limit as usize
                {
                    wait = wait.max(e.limit.period.saturating_sub(now.duration_since(oldest)));
                }
                if wait.is_zero() {
                    w.recent.push_back(now);
                    w.last = Some(now);
                    return Ok(());
                }
                wait
            };
            if Instant::now() + wait > deadline {
                return Err(IndexerError::RateLimited { wait });
            }
            tokio::time::sleep(wait).await;
        }
    }

    async fn run(&self, id: i64, op: Op, query: &SearchQuery, timeout: Duration) -> IndexerSearchResult {
        let start = Instant::now();
        let Some(e) = self.entry(id) else {
            return IndexerSearchResult {
                indexer_id: id,
                name: String::new(),
                result: Err(IndexerError::NotFound(id)),
                elapsed: Duration::ZERO,
            };
        };
        let done = |result| IndexerSearchResult {
            indexer_id: id,
            name: e.name.clone(),
            result,
            elapsed: start.elapsed(),
        };
        if op != Op::Test
            && let Some(until) = self.disabled_until(&e)
        {
            return done(Err(IndexerError::Disabled { until }));
        }
        let deadline = start + timeout;
        if let Err(err) = Self::acquire(&e, deadline).await {
            return done(Err(err));
        }
        let counter = if op == Op::Rss {
            &e.counters.rss
        } else {
            &e.counters.queries
        };
        counter.fetch_add(1, Ordering::Relaxed);
        let call_start = Instant::now();
        let fut = async {
            match op {
                Op::Search => e.indexer.search(query).await,
                Op::Rss => e.indexer.rss(&query.categories).await,
                Op::Test => e.indexer.test().await.map(|()| Vec::new()),
            }
        };
        let result = match tokio::time::timeout_at(deadline, fut).await {
            Ok(r) => r,
            Err(_) => Err(IndexerError::Timeout),
        };
        self.record(&e, result.as_ref().map(|_| ()), call_start.elapsed());
        done(result)
    }

    /// Searches the given indexers concurrently; each gets `timeout` (including any
    /// rate-limit wait). A slow or failing indexer never delays the others' results.
    pub async fn search(
        &self,
        ids: &[i64],
        query: &SearchQuery,
        timeout: Duration,
    ) -> Vec<IndexerSearchResult> {
        futures::future::join_all(ids.iter().map(|&id| self.run(id, Op::Search, query, timeout))).await
    }

    /// RSS fetch across indexers, same semantics as [`Self::search`].
    pub async fn rss(&self, ids: &[i64], categories: &[u32], timeout: Duration) -> Vec<IndexerSearchResult> {
        let q = SearchQuery {
            categories: categories.to_vec(),
            ..SearchQuery::default()
        };
        futures::future::join_all(ids.iter().map(|&id| self.run(id, Op::Rss, &q, timeout))).await
    }

    /// Tests one indexer, ignoring (and on success clearing) any back-off.
    pub async fn test(&self, id: i64, timeout: Duration) -> Result<(), IndexerError> {
        self.run(id, Op::Test, &SearchQuery::default(), timeout)
            .await
            .result
            .map(|_| ())
    }

    /// Fetches a release file through the indexer and counts it as a grab.
    pub async fn download(&self, id: i64, url: &str) -> Result<Vec<u8>, IndexerError> {
        let e = self.entry(id).ok_or(IndexerError::NotFound(id))?;
        let start = Instant::now();
        let res = e.indexer.download(url).await;
        match &res {
            Ok(_) | Err(IndexerError::MagnetRedirect(_)) => {
                e.counters.grabs.fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => self.record(&e, Err(err), start.elapsed()),
        }
        res
    }
}
