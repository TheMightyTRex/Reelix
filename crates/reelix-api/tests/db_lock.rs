//! RFX-71: the database must not lock API-key requests out while a long write runs.
//!
//! A big radio show (a daily drama has over a thousand episodes) refreshed in long write
//! transactions, and every API-key request wrote `last_used_at`. The request's write waited for
//! the refresh past SQLite's 5 second busy timeout and failed with "database is locked" (500), so
//! Starboard's pages and Reelix's own failed until the refresh ended.
//!
//! These tests use a real database file (an in-memory test database has a single connection, so
//! it cannot show a lock), in both journal modes Reelix runs in: the write-ahead log, and the
//! rollback journal it falls back to on file systems that cannot share memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_jobs::JobSystem;
use reelix_radio::provider::async_trait;
use reelix_radio::{
    EpisodeData, ProviderError, ProviderRegistry, RadioDeps, RadioProvider, RadioService, SearchPrefs,
    SeriesData, ShowData, ShowFetch, ShowHit,
};
use serde_json::{Value, json};
use sqlx::Connection;
use sqlx::sqlite::SqliteConnectOptions;
use tower::ServiceExt;

/// Longer than SQLite's busy timeout (5 s), so a request that has to write fails while it is held.
const HOLD: Duration = Duration::from_secs(7);
/// An API-key read must answer well inside this, whatever else is writing.
const READ_LIMIT: Duration = Duration::from_secs(2);
/// The longest another writer may wait for the database while a refresh runs.
const WRITE_WAIT_LIMIT: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug)]
enum Journal {
    Wal,
    Rollback,
}

struct TestApp {
    router: Router,
    db: Db,
    radio: Arc<RadioService>,
    file: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

/// A daily drama: `episodes` episodes, one a day, in one series.
struct LongShow {
    episodes: u32,
    renamed: AtomicBool,
}

impl LongShow {
    fn data(&self) -> ShowData {
        let renamed = self.renamed.load(Ordering::SeqCst);
        ShowData {
            external_id: "p-long".into(),
            title: "A daily drama".into(),
            series: vec![SeriesData {
                external_id: "s1".into(),
                number: Some(1),
                ..SeriesData::default()
            }],
            episodes: (0..self.episodes)
                .map(|i| EpisodeData {
                    external_id: format!("e{i:05}"),
                    series_external_id: "s1".into(),
                    title: if renamed {
                        format!("Episode {i}, renamed")
                    } else {
                        format!("Episode {i}")
                    },
                    first_broadcast: Some(
                        Utc.with_ymd_and_hms(2020, 1, 1, 19, 0, 0).unwrap()
                            + chrono::Duration::days(i64::from(i)),
                    ),
                    ..EpisodeData::default()
                })
                .collect(),
            complete: true,
            ..ShowData::default()
        }
    }
}

#[async_trait]
impl RadioProvider for LongShow {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn name(&self) -> &'static str {
        "Fake"
    }
    fn supports_search(&self) -> bool {
        true
    }
    async fn search(&self, _q: &str, _prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError> {
        Ok(vec![self.data().hit("fake")])
    }
    async fn preview(&self, _link: &str) -> Result<Option<ShowData>, ProviderError> {
        Ok(None)
    }
    async fn show(&self, _id: &str, _fetch: &ShowFetch) -> Result<ShowData, ProviderError> {
        Ok(self.data())
    }
}

async fn app(journal: Journal, show: Arc<LongShow>) -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("reelix.db");
    if let Journal::Rollback = journal {
        // The marker Reelix leaves when the write-ahead log cannot be used in its folder.
        let mut marker = file.clone().into_os_string();
        marker.push(".no-wal");
        std::fs::write(marker, "test").unwrap();
    }
    let db = Db::connect(&format!("sqlite://{}", file.display()))
        .await
        .unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(db.pool())
        .await
        .unwrap();
    match journal {
        Journal::Wal => assert_eq!(mode, "wal"),
        Journal::Rollback => assert_eq!(mode, "delete"),
    }
    let events = EventBus::default();
    let system = JobSystem::builder(db.clone(), events.clone()).build();
    let radio = Arc::new(RadioService::new(RadioDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        providers: ProviderRegistry::default().with(show),
        library_sync: None,
    }));
    radio.set_queue(system.queue());
    let config = ApiConfig {
        url_base: String::new(),
        data_dir: dir.path().to_path_buf(),
        database: "sqlite".into(),
        start_time: Utc::now(),
    };
    let state = AppState::new(
        db.clone(),
        events,
        system.queue(),
        SecretBox::from_key([7u8; 32]),
        config,
    )
    .with_extension(radio.clone());
    TestApp {
        router: reelix_api::router(state),
        db,
        radio,
        file,
        _dir: dir,
    }
}

async fn send(app: &TestApp, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// Sets up the admin and returns a new API key of theirs.
async fn api_key(app: &TestApp) -> String {
    let setup = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"username": "admin", "password": "correct-horse-battery"}).to_string(),
        ))
        .unwrap();
    let res = app.router.clone().oneshot(setup).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let cookie = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let create = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/apikeys")
        .header(header::COOKIE, cookie)
        .header("x-reelix-request", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"name": "Starboard"}).to_string()))
        .unwrap();
    let (status, body) = send(app, create).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["key"].as_str().unwrap().to_owned()
}

/// One API-key read, as Starboard makes them; returns the status and how long it took.
async fn key_read(app: &TestApp, key: &str, uri: &str) -> (StatusCode, Duration) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("x-api-key", key)
        .body(Body::empty())
        .unwrap();
    let started = Instant::now();
    let (status, _) = send(app, req).await;
    (status, started.elapsed())
}

/// A connection of its own to the database file, like a second process or a long job.
async fn other_connection(app: &TestApp) -> sqlx::SqliteConnection {
    let opts = SqliteConnectOptions::new()
        .filename(&app.file)
        .busy_timeout(Duration::from_secs(30));
    sqlx::SqliteConnection::connect_with(&opts).await.unwrap()
}

/// API-key requests made while another writer holds the write lock for longer than the busy
/// timeout. Before RFX-71 the first request of a key wrote `last_used_at`, waited 5 s and failed
/// with 500; now a request with a key writes nothing.
async fn key_reads_while_the_write_lock_is_held(journal: Journal) {
    let app = app(
        journal,
        Arc::new(LongShow {
            episodes: 3,
            renamed: AtomicBool::new(false),
        }),
    )
    .await;
    let key = api_key(&app).await;

    let mut writer = other_connection(&app).await;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut writer).await.unwrap();
    sqlx::query("UPDATE users SET updated_at = updated_at")
        .execute(&mut writer)
        .await
        .unwrap();
    let held_since = Instant::now();

    let mut reads = 0;
    while held_since.elapsed() < HOLD {
        for uri in ["/api/v1/radio/shows", "/api/v1/auth/me", "/api/v1/radio/settings"] {
            let (status, took) = key_read(&app, &key, uri).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{journal:?}: {uri} while the write lock was held"
            );
            assert!(took < READ_LIMIT, "{journal:?}: {uri} took {took:?}");
            reads += 1;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    sqlx::query("COMMIT").execute(&mut writer).await.unwrap();
    assert!(reads >= 10, "{journal:?}: only {reads} reads were made");

    // The key's use is kept in memory: shown at once, written by the next flush.
    let listed = app.db.api_keys().list_for_user(1).await.unwrap();
    assert!(
        listed[0].last_used_at.is_some(),
        "{journal:?}: use is shown before it is written"
    );
    let stored: Option<i64> = sqlx::query_scalar("SELECT last_used_at FROM api_keys")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert!(stored.is_none(), "{journal:?}: a request wrote last_used_at");
    assert_eq!(app.db.flush_key_use().await.unwrap(), 1);
    let stored: Option<i64> = sqlx::query_scalar("SELECT last_used_at FROM api_keys")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert!(stored.is_some(), "{journal:?}: the flush wrote last_used_at");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn key_reads_while_the_write_lock_is_held_wal() {
    key_reads_while_the_write_lock_is_held(Journal::Wal).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn key_reads_while_the_write_lock_is_held_rollback_journal() {
    key_reads_while_the_write_lock_is_held(Journal::Rollback).await;
}

/// A radio show with 1,500 episodes is followed and then refreshed with every episode changed,
/// while API-key requests are made and another writer keeps asking for the write lock. Every
/// request answers, and no writer waits longer than [`WRITE_WAIT_LIMIT`]: the refresh never holds
/// the write lock for long.
async fn a_long_radio_refresh_does_not_lock_the_database(journal: Journal) {
    const EPISODES: u32 = 1_500;
    let show = Arc::new(LongShow {
        episodes: EPISODES,
        renamed: AtomicBool::new(false),
    });
    let app = app(journal, show.clone()).await;
    let key = api_key(&app).await;

    let running = Arc::new(AtomicBool::new(true));
    // A second writer, like the job runner or the key-use flush: how long does it wait for the lock?
    let probe = {
        let mut conn = other_connection(&app).await;
        let running = running.clone();
        tokio::spawn(async move {
            let mut longest = Duration::ZERO;
            let mut writes = 0u32;
            while running.load(Ordering::SeqCst) {
                let started = Instant::now();
                sqlx::query("BEGIN IMMEDIATE").execute(&mut conn).await.unwrap();
                longest = longest.max(started.elapsed());
                sqlx::query("UPDATE users SET updated_at = updated_at")
                    .execute(&mut conn)
                    .await
                    .unwrap();
                sqlx::query("COMMIT").execute(&mut conn).await.unwrap();
                writes += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            (longest, writes)
        })
    };

    let refresh = {
        let radio = app.radio.clone();
        let show = show.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let added = radio
                .add_show(reelix_api_types::radio::AddRadioShowRequest {
                    provider: Some("fake".into()),
                    external_id: Some("p-long".into()),
                    monitored: true,
                    ..Default::default()
                })
                .await
                .unwrap();
            show.renamed.store(true, Ordering::SeqCst);
            radio.refresh_show(added.id).await.unwrap();
            (added.id, started.elapsed())
        })
    };

    let mut reads = 0;
    let mut slowest = Duration::ZERO;
    while !refresh.is_finished() {
        for uri in ["/api/v1/radio/shows", "/api/v1/radio/settings", "/api/v1/auth/me"] {
            let (status, took) = key_read(&app, &key, uri).await;
            assert_eq!(status, StatusCode::OK, "{journal:?}: {uri} during the refresh");
            slowest = slowest.max(took);
            reads += 1;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (show_id, refresh_took) = refresh.await.unwrap();
    running.store(false, Ordering::SeqCst);
    let (longest_wait, writes) = probe.await.unwrap();

    eprintln!(
        "{journal:?}: refresh of {EPISODES} episodes took {refresh_took:?}; {reads} key reads (slowest \
         {slowest:?}); {writes} probe writes (longest wait {longest_wait:?})"
    );
    assert!(
        reads > 0,
        "{journal:?}: the refresh ended before any request was made"
    );
    assert!(
        slowest < READ_LIMIT,
        "{journal:?}: slowest key read took {slowest:?}"
    );
    assert!(
        longest_wait < WRITE_WAIT_LIMIT,
        "{journal:?}: another writer waited {longest_wait:?} for the database"
    );
    let eps = app.radio.show_episodes(show_id).await.unwrap().episodes;
    assert_eq!(eps.len(), EPISODES as usize);
    assert!(eps.iter().all(|e| e.title.ends_with("renamed")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_radio_refresh_does_not_lock_the_database_wal() {
    a_long_radio_refresh_does_not_lock_the_database(Journal::Wal).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_radio_refresh_does_not_lock_the_database_rollback_journal() {
    a_long_radio_refresh_does_not_lock_the_database(Journal::Rollback).await;
}
