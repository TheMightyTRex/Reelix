use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reelix_core::Protocol;
use reelix_indexers::manager::backoff_for;
use reelix_indexers::{
    Indexer, IndexerCapabilities, IndexerError, IndexerManager, IndexerStatus, RateLimit, ReleaseInfo,
    SearchQuery,
};

#[derive(Clone, Copy)]
enum Mode {
    Ok,
    Slow(Duration),
    Fail,
    Limited,
}

struct Fake {
    id: i64,
    mode: Mutex<Mode>,
    calls: AtomicU32,
}

impl Fake {
    fn new(id: i64, mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            id,
            mode: Mutex::new(mode),
            calls: AtomicU32::new(0),
        })
    }

    fn set(&self, m: Mode) {
        *self.mode.lock().unwrap() = m;
    }
}

#[async_trait]
impl Indexer for Fake {
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    async fn capabilities(&self) -> Result<IndexerCapabilities, IndexerError> {
        Ok(IndexerCapabilities::default())
    }
    async fn search(&self, _q: &SearchQuery) -> Result<Vec<ReleaseInfo>, IndexerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mode = *self.mode.lock().unwrap();
        match mode {
            Mode::Ok => {
                let mut r = ReleaseInfo::new(Protocol::Torrent, self.id, "fake");
                r.title = format!("Release from {}", self.id);
                Ok(vec![r])
            }
            Mode::Slow(d) => {
                tokio::time::sleep(d).await;
                Ok(Vec::new())
            }
            Mode::Fail => Err(IndexerError::Status { status: 502 }),
            Mode::Limited => Err(IndexerError::RequestLimit {
                retry_after: Some(Duration::from_secs(7200)),
            }),
        }
    }
    async fn rss(&self, _c: &[u32]) -> Result<Vec<ReleaseInfo>, IndexerError> {
        self.search(&SearchQuery::default()).await
    }
    async fn test(&self) -> Result<(), IndexerError> {
        self.search(&SearchQuery::default()).await.map(|_| ())
    }
    async fn download(&self, _url: &str) -> Result<Vec<u8>, IndexerError> {
        Ok(b"torrent".to_vec())
    }
}

fn clock() -> (Arc<Mutex<DateTime<Utc>>>, IndexerManager) {
    let now = Arc::new(Mutex::new(Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap()));
    let c = now.clone();
    (
        now,
        IndexerManager::with_clock(Arc::new(move || *c.lock().unwrap())),
    )
}

#[tokio::test]
async fn fan_out_isolates_slow_and_failing_indexers() {
    let m = IndexerManager::new();
    m.register(1, "fast", Fake::new(1, Mode::Ok), RateLimit::default());
    m.register(
        2,
        "slow",
        Fake::new(2, Mode::Slow(Duration::from_secs(30))),
        RateLimit::default(),
    );
    m.register(3, "broken", Fake::new(3, Mode::Fail), RateLimit::default());
    let start = std::time::Instant::now();
    let res = m
        .search(
            &[1, 2, 3, 99],
            &SearchQuery::basic("x"),
            Duration::from_millis(300),
        )
        .await;
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(res.len(), 4);
    assert_eq!(res[0].result.as_ref().unwrap().len(), 1);
    assert_eq!(res[0].name, "fast");
    assert!(matches!(res[1].result, Err(IndexerError::Timeout)));
    assert!(matches!(res[2].result, Err(IndexerError::Status { status: 502 })));
    assert!(matches!(res[3].result, Err(IndexerError::NotFound(99))));

    let stats = m.stats();
    assert_eq!(stats.len(), 3);
    assert_eq!((stats[0].queries, stats[0].failures), (1, 0));
    assert_eq!(stats[1].failures, 1, "timeouts count as failures");
    assert_eq!(stats[2].failures, 1);
    assert!(
        stats[2]
            .status
            .last_failure_reason
            .as_deref()
            .unwrap()
            .contains("502")
    );
}

#[tokio::test]
async fn failures_back_off_escalate_and_auto_reenable() {
    let (now, m) = clock();
    let fake = Fake::new(1, Mode::Fail);
    m.register(1, "a", fake.clone(), RateLimit::default());
    let q = SearchQuery::basic("x");
    let t = Duration::from_secs(5);

    m.search(&[1], &q, t).await;
    let st = m.status(1).unwrap();
    assert_eq!(st.failure_count, 1);
    assert_eq!(
        st.disabled_until,
        Some(*now.lock().unwrap() + chrono::Duration::minutes(5))
    );
    assert!(!m.is_available(1));
    let r = m.search(&[1], &q, t).await;
    assert!(matches!(r[0].result, Err(IndexerError::Disabled { .. })));
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        1,
        "disabled indexer is not queried"
    );

    *now.lock().unwrap() += chrono::Duration::minutes(6);
    assert!(m.is_available(1), "re-enabled automatically after the back-off");
    m.search(&[1], &q, t).await;
    let st = m.status(1).unwrap();
    assert_eq!(st.failure_count, 2);
    assert_eq!(
        st.disabled_until,
        Some(*now.lock().unwrap() + chrono::Duration::minutes(15))
    );

    // A test bypasses the back-off and a success clears it.
    fake.set(Mode::Ok);
    m.test(1, t).await.unwrap();
    assert_eq!(m.status(1).unwrap(), IndexerStatus::default());

    // Indexer-reported limits honour Retry-After.
    fake.set(Mode::Limited);
    m.search(&[1], &q, t).await;
    assert_eq!(
        m.status(1).unwrap().disabled_until,
        Some(*now.lock().unwrap() + chrono::Duration::hours(2))
    );

    assert_eq!(backoff_for(1), Duration::from_secs(300));
    assert_eq!(backoff_for(50), Duration::from_secs(86_400));

    // Persisted state can be restored and survives re-registration.
    m.restore_status(
        1,
        IndexerStatus {
            failure_count: 8,
            disabled_until: Some(*now.lock().unwrap() + chrono::Duration::hours(24)),
            last_failure_reason: None,
        },
    );
    m.register(1, "a", fake.clone(), RateLimit::default());
    assert_eq!(m.status(1).unwrap().failure_count, 8);
    m.reenable(1);
    assert!(m.is_available(1));
}

#[tokio::test(start_paused = true)]
async fn rate_limits_wait_or_fail_within_timeout() {
    let m = IndexerManager::new();
    let limit = RateLimit {
        query_limit: Some(2),
        period: Duration::from_secs(10),
        min_interval: None,
    };
    m.register(1, "a", Fake::new(1, Mode::Ok), limit);
    let q = SearchQuery::basic("x");
    for _ in 0..2 {
        assert!(m.search(&[1], &q, Duration::from_secs(1)).await[0].result.is_ok());
    }
    let r = m.search(&[1], &q, Duration::from_secs(1)).await;
    assert!(matches!(r[0].result, Err(IndexerError::RateLimited { .. })));
    assert!(
        m.status(1).unwrap().failure_count == 0,
        "local rate limiting is not a failure"
    );

    let start = tokio::time::Instant::now();
    assert!(
        m.search(&[1], &q, Duration::from_secs(30)).await[0]
            .result
            .is_ok()
    );
    assert!(
        start.elapsed() >= Duration::from_secs(9),
        "waited for the window: {:?}",
        start.elapsed()
    );

    let m2 = IndexerManager::new();
    m2.register(
        2,
        "b",
        Fake::new(2, Mode::Ok),
        RateLimit {
            min_interval: Some(Duration::from_secs(2)),
            ..RateLimit::default()
        },
    );
    let start = tokio::time::Instant::now();
    for _ in 0..3 {
        assert!(
            m2.search(&[2], &q, Duration::from_secs(10)).await[0]
                .result
                .is_ok()
        );
    }
    assert!(start.elapsed() >= Duration::from_secs(4));
}

#[tokio::test]
async fn grabs_and_rss_are_counted() {
    let m = IndexerManager::new();
    m.register(1, "a", Fake::new(1, Mode::Ok), RateLimit::default());
    assert_eq!(m.download(1, "/x").await.unwrap(), b"torrent");
    assert_eq!(
        m.rss(&[1], &[2000], Duration::from_secs(1)).await[0]
            .result
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    let s = &m.stats()[0];
    assert_eq!((s.grabs, s.rss_queries, s.queries), (1, 1, 0));
    assert!(matches!(
        m.download(7, "/x").await,
        Err(IndexerError::NotFound(7))
    ));
    assert_eq!(m.ids(), [1]);
    assert!(m.unregister(1) && m.get(1).is_none());
}
