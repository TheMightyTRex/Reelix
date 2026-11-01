//! Performance pass: a library of 10k movies, 2k series (100k episodes) and 2k artists
//! (20k albums, 100k tracks) seeded straight into SQLite, then the hot API endpoints timed
//! against the targets in docs/design.md (library pages < 150 ms p95).
//!
//! Ignored by default (seeding takes a while); run with
//! `cargo test --release -p reelix-api --test perf -- --ignored --nocapture`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use reelix_metadata::MetaError;
use reelix_metadata::music::*;
use reelix_music::{MusicDeps, MusicService};
use tower::ServiceExt;

const MOVIES: i64 = 10_000;
const SERIES: i64 = 2_000;
const SEASONS: i64 = 5;
const EPISODES_PER_SEASON: i64 = 10;
const ARTISTS: i64 = 2_000;
const ALBUMS_PER_ARTIST: i64 = 10;
const TRACKS_PER_ALBUM: i64 = 5;

struct NoMusicBrainz;

#[async_trait]
impl MusicMetadataProvider for NoMusicBrainz {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn search_artists(&self, _q: &str) -> Result<Vec<ArtistSearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn search_release_groups(
        &self,
        _q: &str,
        _a: Option<&str>,
    ) -> Result<Vec<ReleaseGroupSearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn artist(&self, _id: &str) -> Result<ArtistMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn release_group(&self, _id: &str) -> Result<ReleaseGroupMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn releases(&self, _id: &str) -> Result<Vec<ReleaseMetadata>, MetaError> {
        Ok(vec![])
    }
    async fn release(&self, _id: &str) -> Result<ReleaseMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn cover_art(&self, _id: &str) -> Result<Option<CoverArt>, MetaError> {
        Ok(None)
    }
}

async fn exec(db: &Db, sql: &str) {
    sqlx::query(sql)
        .execute(db.pool())
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
}

/// Inserts the library with recursive CTEs (seconds, not minutes).
async fn seed(db: &Db, root: &str) {
    let now = chrono::Utc::now().timestamp_millis();
    let profile: i64 =
        sqlx::query_scalar("SELECT id FROM quality_profiles WHERE media_kind = 'video' ORDER BY id LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let music_profile: i64 =
        sqlx::query_scalar("SELECT id FROM quality_profiles WHERE media_kind = 'music' ORDER BY id LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    for (id, kind) in [(1, "movie"), (2, "series"), (3, "music")] {
        exec(
            db,
            &format!(
                "INSERT INTO root_folders (id, path, media_kind, created_at, updated_at) VALUES ({id}, '{root}/{kind}', '{kind}', {now}, {now})"
            ),
        )
        .await;
    }
    let quality = r#"{"quality":"Bluray-1080p","revision":{"version":1,"real":0,"is_repack":false}}"#;
    let today = chrono::Utc::now().date_naive();
    // Release dates spread over ±5 years so the calendar and wanted lists have work to do.
    exec(
        db,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {MOVIES})
             INSERT INTO movies (id, tmdb_id, imdb_id, title, sort_title, clean_title, year, status, overview,
                runtime, genres, images, ratings, in_cinemas, digital_release, monitored, minimum_availability,
                quality_profile_id, root_folder_id, path, created_at, updated_at)
             SELECT i, 100000 + i, printf('tt%07d', i), 'Movie ' || i, 'movie ' || i, 'movie' || i,
                1990 + (i % 36), 'released', 'An overview of movie ' || i, 110, '[\"Drama\"]',
                '{{\"poster\":\"https://image.tmdb.org/t/p/w500/p' || i || '.jpg\"}}', '{{}}',
                date('{today}', printf('%+d days', (i % 3650) - 1825)),
                date('{today}', printf('%+d days', (i % 3650) - 1800)),
                (i % 5) != 0, 'released', {profile}, 1, '{root}/movie/Movie ' || i, {now} - i, {now}
             FROM n"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "INSERT INTO movie_files (movie_id, relative_path, size, quality, date_added, created_at, updated_at)
             SELECT id, 'Movie ' || id || '.mkv', 8000000000, '{quality}', {now}, {now}, {now}
             FROM movies WHERE id % 2 = 0"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {SERIES})
             INSERT INTO series (id, tmdb_id, tvdb_id, title, sort_title, clean_title, year, status, network, genres,
                images, ratings, first_aired, monitored, quality_profile_id, root_folder_id, path, created_at, updated_at)
             SELECT i, 200000 + i, 300000 + i, 'Show ' || i, 'show ' || i, 'show' || i, 2000 + (i % 25),
                'continuing', 'Network', '[]', '{{}}', '{{}}', '2010-01-01', (i % 4) != 0, {profile}, 2,
                '{root}/series/Show ' || i, {now}, {now}
             FROM n"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < {SEASONS})
             INSERT INTO seasons (series_id, season_number, monitored)
             SELECT series.id, s.n, 1 FROM series, s"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "INSERT INTO episode_files (series_id, season_number, relative_path, size, quality, date_added, created_at, updated_at)
             SELECT series_id, season_number, 'S' || season_number || '.mkv', 2000000000, '{quality}', {now}, {now}, {now}
             FROM seasons WHERE season_number <= 3"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "WITH RECURSIVE e(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM e WHERE n < {EPISODES_PER_SEASON})
             INSERT INTO episodes (series_id, season_number, episode_number, title, air_date, runtime, monitored,
                episode_file_id, created_at, updated_at)
             SELECT s.series_id, s.season_number, e.n, 'Episode ' || e.n,
                date('{today}', printf('%+d days', (s.series_id * 7 + s.season_number * 70 + e.n * 7) % 2000 - 1600)),
                45, 1,
                (SELECT f.id FROM episode_files f WHERE f.series_id = s.series_id AND f.season_number = s.season_number),
                {now}, {now}
             FROM seasons s, e"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {ARTISTS})
             INSERT INTO artists (id, musicbrainz_id, name, sort_name, clean_name, genres, images, links, monitored,
                quality_profile_id, metadata_profile_id, root_folder_id, path, created_at, updated_at)
             SELECT i, printf('00000000-0000-0000-0000-%012d', i), 'Artist ' || i, 'artist ' || i, 'artist' || i,
                '[]', '{{}}', '[]', 1, {music_profile}, 1, 3, '{root}/music/Artist ' || i, {now}, {now}
             FROM n"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "WITH RECURSIVE a(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM a WHERE n < {ALBUMS_PER_ARTIST})
             INSERT INTO albums (artist_id, musicbrainz_release_group_id, title, clean_title, album_type, release_date,
                monitored, created_at, updated_at)
             SELECT artists.id, printf('10000000-0000-0000-%04d-%012d', a.n, artists.id), 'Album ' || a.n,
                'album' || a.n, 'album', date('2000-01-01', printf('+%d days', a.n * 300)), 1, {now}, {now}
             FROM artists, a"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "INSERT INTO album_releases (album_id, musicbrainz_release_id, title, status, track_count, created_at, updated_at)
             SELECT id, printf('20000000-0000-0000-0000-%012d', id), title, 'official', {TRACKS_PER_ALBUM}, {now}, {now} FROM albums"
        ),
    )
    .await;
    exec(db, "UPDATE albums SET selected_release_id = (SELECT r.id FROM album_releases r WHERE r.album_id = albums.id)").await;
    let mp3 = r#"{"quality":"MP3-320","revision":{"version":1,"real":0,"is_repack":false}}"#;
    exec(
        db,
        &format!(
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < {TRACKS_PER_ALBUM})
             INSERT INTO track_files (album_id, relative_path, size, quality, date_added, created_at, updated_at)
             SELECT albums.id, albums.title || '/' || t.n || '.mp3', 10000000, '{mp3}', {now}, {now}, {now}
             FROM albums, t WHERE albums.id % 2 = 0"
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < {TRACKS_PER_ALBUM})
             INSERT INTO tracks (album_id, release_id, disc_number, track_number, title, duration_ms, track_file_id,
                created_at, updated_at)
             SELECT r.album_id, r.id, 1, t.n, 'Track ' || t.n, 200000,
                (SELECT f.id FROM track_files f WHERE f.album_id = r.album_id AND f.relative_path LIKE '%/' || t.n || '.mp3'),
                {now}, {now}
             FROM album_releases r, t"
        ),
    )
    .await;
    exec(db, "ANALYZE").await;
}

async fn app() -> (Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("perf.db").display());
    let db = Db::connect(&url).await.unwrap();
    let events = EventBus::default();
    let system = JobSystem::builder(db.clone(), events.clone()).build();
    let config = ApiConfig {
        url_base: String::new(),
        data_dir: dir.path().to_path_buf(),
        database: "sqlite".into(),
        start_time: chrono::Utc::now(),
    };
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    // Ensure every family's default profiles exist before seeding.
    for family in [
        reelix_parser::QualityFamily::Video,
        reelix_parser::QualityFamily::Music,
    ] {
        db.quality_profiles().list_family(family).await.unwrap();
    }
    let started = Instant::now();
    seed(&db, &dir.path().display().to_string()).await;
    eprintln!("seeded in {:?}", started.elapsed());
    let music = Arc::new(MusicService::new(MusicDeps {
        db: db.clone(),
        events: events.clone(),
        http: reqwest::Client::new(),
        library: library.clone(),
        metadata: Arc::new(NoMusicBrainz),
    }));
    let user = db
        .users()
        .create(NewUser {
            username: "bench".into(),
            email: None,
            display_name: None,
            password_hash: None,
            auth_source: AuthSource::Local,
            permissions: Permissions::ADMIN,
        })
        .await
        .unwrap();
    let key = reelix_api::auth::random_token();
    db.api_keys()
        .create(user.id, "bench", &key[..8], &reelix_api::auth::sha256_hex(&key))
        .await
        .unwrap();
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([1u8; 32]), config)
        .with_extension(library)
        .with_extension(music);
    (reelix_api::router(state), key, dir)
}

async fn time(router: &Router, key: &str, uri: &str, runs: usize) -> (Duration, Duration, usize) {
    let mut samples = Vec::with_capacity(runs);
    let mut bytes = 0;
    for _ in 0..runs {
        let req = Request::builder()
            .uri(uri)
            .header("x-api-key", key)
            .body(Body::empty())
            .unwrap();
        let start = Instant::now();
        let res = router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        samples.push(start.elapsed());
        assert_eq!(
            status,
            StatusCode::OK,
            "{uri}: {}",
            String::from_utf8_lossy(&body)
        );
        bytes = body.len();
    }
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
    (p50, p95, bytes)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "seeds a large library; run explicitly in release mode"]
async fn library_endpoints_meet_targets() {
    let (router, key, _dir) = app().await;
    let today = chrono::Utc::now().date_naive();
    let calendar = format!(
        "/api/v1/calendar?start={}&end={}",
        today - chrono::Days::new(7),
        today + chrono::Days::new(30)
    );
    // (endpoint, p95 target) — `None` = measured and reported only.
    let cases: Vec<(String, Option<u64>)> = vec![
        ("/api/v1/movies?page=1&page_size=50".into(), Some(150)),
        (
            "/api/v1/movies?page=100&page_size=50&sort=title".into(),
            Some(150),
        ),
        (
            "/api/v1/movies?page=1&page_size=50&sort=added&dir=desc&monitored=true".into(),
            Some(150),
        ),
        (
            "/api/v1/movies?page=1&page_size=50&q=movie%2042".into(),
            Some(150),
        ),
        (
            "/api/v1/movies?page=1&page_size=50&missing=true".into(),
            Some(150),
        ),
        ("/api/v1/series?page=1&page_size=50".into(), Some(150)),
        ("/api/v1/series?page=20&page_size=50&sort=title".into(), Some(150)),
        ("/api/v1/artists?page=1&page_size=50".into(), Some(150)),
        ("/api/v1/artists?page=20&page_size=50".into(), Some(150)),
        (
            "/api/v1/wanted/missing?kind=movie&page=1&page_size=20".into(),
            Some(150),
        ),
        (
            "/api/v1/wanted/missing?kind=episode&page=1&page_size=20".into(),
            Some(150),
        ),
        (calendar, Some(150)),
        ("/api/v1/albums/wanted?kind=missing&page_size=1".into(), None),
        ("/radarr/api/v3/movie".into(), None),
        ("/sonarr/api/v3/series".into(), None),
    ];
    let mut failures = Vec::new();
    eprintln!("{:<75} {:>9} {:>9} {:>10}", "endpoint", "p50", "p95", "bytes");
    for (uri, target) in &cases {
        // Warm up (statement cache, page cache), then measure.
        time(&router, &key, uri, 2).await;
        let runs = if target.is_some() { 20 } else { 5 };
        let (p50, p95, bytes) = time(&router, &key, uri, runs).await;
        eprintln!(
            "{uri:<75} {:>7.1}ms {:>7.1}ms {bytes:>10}",
            p50.as_secs_f64() * 1e3,
            p95.as_secs_f64() * 1e3
        );
        if let Some(t) = target
            && p95 > Duration::from_millis(*t)
        {
            failures.push(format!("{uri}: p95 {p95:?} > {t} ms"));
        }
    }
    if cfg!(debug_assertions) {
        eprintln!("debug build: targets not enforced");
    } else {
        assert!(failures.is_empty(), "over target:\n{}", failures.join("\n"));
    }
}
