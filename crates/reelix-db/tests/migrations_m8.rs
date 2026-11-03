//! Upgrade test for the M8/M9 migrations: builds a populated database at version 0007, then
//! applies 0008–0010 (which rebuild `root_folders` and `requests`) and checks that every row,
//! value, constraint, index and foreign key survives.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use reelix_db::Db;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations/sqlite")
}

/// Copies the migrations with version ≤ `last` into `dst`.
fn copy_migrations_upto(dst: &Path, last: u32) {
    let mut n = 0;
    for entry in std::fs::read_dir(migrations_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some(version) = name.split('_').next().and_then(|v| v.parse::<u32>().ok()) else {
            continue;
        };
        if version <= last {
            std::fs::copy(&path, dst.join(&name)).unwrap();
            n += 1;
        }
    }
    assert_eq!(n, last, "expected migrations 0001..{last:04}");
}

async fn open_pool(path: &Path) -> SqlitePool {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap()
}

async fn exec(pool: &SqlitePool, sql: &str) {
    sqlx::raw_sql(sql).execute(pool).await.unwrap();
}

async fn columns(pool: &SqlitePool, table: &str) -> Vec<String> {
    sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>("name"))
        .collect()
}

/// Every row of `table` rendered as quoted values of `cols`, ordered by id.
async fn snapshot(pool: &SqlitePool, table: &str, cols: &[String]) -> Vec<String> {
    let expr = cols
        .iter()
        .map(|c| format!("quote({c})"))
        .collect::<Vec<_>>()
        .join(" || '|' || ");
    sqlx::query(&format!("SELECT {expr} AS v FROM {table} ORDER BY id"))
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>("v"))
        .collect()
}

async fn index_names(pool: &SqlitePool, table: &str) -> Vec<String> {
    let mut names: Vec<String> = sqlx::query(&format!("PRAGMA index_list({table})"))
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>("name"))
        .filter(|n| !n.starts_with("sqlite_autoindex"))
        .collect();
    names.sort();
    names
}

async fn foreign_keys(pool: &SqlitePool, table: &str) -> Vec<(String, String, String, String)> {
    let mut fks: Vec<_> = sqlx::query(&format!("PRAGMA foreign_key_list({table})"))
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("from"),
                r.get::<String, _>("table"),
                r.get::<String, _>("to"),
                r.get::<String, _>("on_delete"),
            )
        })
        .collect();
    fks.sort();
    fks
}

async fn seq(pool: &SqlitePool, table: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT seq FROM sqlite_sequence WHERE name = ?")
        .bind(table)
        .fetch_optional(pool)
        .await
        .unwrap()
}

const SEED_0007: &str = r#"
INSERT INTO users (id, username, email, display_name, auth_source, external_id, permissions, created_at, updated_at,
                   movie_quota_limit, movie_quota_days, tv_quota_limit, tv_quota_days, plex_token)
VALUES (1, 'admin', 'admin@example.com', 'Admin', 'local', NULL, 1, 1700000000000, 1700000000000, NULL, NULL, NULL, NULL, NULL),
       (2, 'alice', NULL, 'Alice', 'plex', '12345', 96, 1700000001000, 1700000002000, 5, 7, 2, 14, 'enc:token'),
       (3, 'bob', 'bob@example.com', NULL, 'jellyfin', 'b0b-guid', 8288, 1700000003000, 1700000003000, NULL, NULL, NULL, NULL, NULL);

INSERT INTO root_folders (id, path, media_kind, created_at, updated_at)
VALUES (1, '/data/movies', 'movie', 1700000000000, 1700000000000),
       (2, '/data/tv', 'series', 1700000000100, 1700000000200),
       (3, '/data/movies-4k', 'movie', 1700000000300, 1700000000300),
       (4, '/data/old', 'series', 1700000000400, 1700000000400);
-- A deleted root folder: its id must not be handed out again.
DELETE FROM root_folders WHERE id = 4;

INSERT INTO movies (id, tmdb_id, title, sort_title, clean_title, quality_profile_id, root_folder_id, path,
                    added_by_request_id, created_at, updated_at)
VALUES (10, 603, 'The Matrix', 'matrix', 'thematrix', 4, 1, '/data/movies/The Matrix (1999)', 1, 1700000010000, 1700000010000);
INSERT INTO series (id, tmdb_id, title, sort_title, clean_title, quality_profile_id, root_folder_id, path,
                    added_by_request_id, created_at, updated_at)
VALUES (20, 1399, 'Game of Thrones', 'game of thrones', 'gameofthrones', 4, 2, '/data/tv/Game of Thrones', 2, 1700000020000, 1700000020000);

INSERT INTO requests (id, user_id, media_kind, tmdb_id, tvdb_id, imdb_id, title, year, poster, backdrop, seasons, status,
                      is_4k, profile_id, root_folder_id, tags, modified_by, decline_reason, failure_reason, movie_id,
                      series_id, source, created_at, updated_at, approved_at, available_at)
VALUES (1, 2, 'movie', 603, NULL, 'tt0133093', 'The Matrix', 1999, '/p/matrix.jpg', '/b/matrix.jpg', '[]', 'available',
        0, 4, 1, '[1,2]', 1, NULL, NULL, 10, NULL, 'manual', 1700000005000, 1700000011000, 1700000006000, 1700000011000),
       (2, 2, 'series', 1399, 121361, 'tt0944947', 'Game of Thrones', 2011, '/p/got.jpg', NULL, '[1,2,3]',
        'partially_available', 0, 4, 2, '[]', 1, NULL, NULL, NULL, 20, 'watchlist', 1700000007000, 1700000021000,
        1700000008000, NULL),
       (3, 3, 'movie', 550, NULL, NULL, 'Fight Club', 1999, NULL, NULL, '[]', 'declined', 1, NULL, NULL, '[]', 1,
        'Not in 4K', NULL, NULL, NULL, 'manual', 1700000030000, 1700000031000, NULL, NULL),
       (4, 3, 'series', 1396, 81189, 'tt0903747', 'Breaking Bad', 2008, NULL, NULL, '[5]', 'failed', 0, 7, 3, '[3]',
        NULL, NULL, 'no root folder', NULL, NULL, 'manual', 1700000040000, 1700000041000, 1700000040500, NULL),
       (5, 2, 'movie', 27205, NULL, 'tt1375666', 'Inception', 2010, NULL, NULL, '[]', 'pending', 0, NULL, NULL, '[]',
        NULL, NULL, NULL, NULL, NULL, 'manual', 1700000050000, 1700000050000, NULL, NULL),
       (6, 3, 'movie', 680, NULL, NULL, 'Pulp Fiction', 1994, NULL, NULL, '[]', 'pending', 0, NULL, NULL, '[]',
        NULL, NULL, NULL, NULL, NULL, 'manual', 1700000060000, 1700000060000, NULL, NULL);
-- The newest request was deleted: id 6 must not be reused after the rebuild.
DELETE FROM requests WHERE id = 6;

INSERT INTO quality_profiles (id, name, items, cutoff, created_at, updated_at)
VALUES (4, 'HD-1080p', '[]', 21, 1700000000000, 1700000000000);
INSERT INTO settings (key, value, updated_at) VALUES ('seeded.quality_profiles', 'true', 1700000000000);

INSERT INTO history (id, event_type, movie_id, source_title, quality, date)
VALUES (1, 'grabbed', 10, 'The.Matrix.1999.1080p.BluRay.x264-GRP', '{"quality":"Bluray-1080p"}', 1700000009000);
INSERT INTO blocklist (id, series_id, episode_ids, source_title, date)
VALUES (1, 20, '[100,101]', 'Game.of.Thrones.S01.1080p-BAD', 1700000022000);
INSERT INTO tracked_downloads (id, download_client_id, download_id, protocol, title, movie_id, state, added, updated)
VALUES (1, 1, 'abc123', 'torrent', 'The.Matrix.1999.1080p.BluRay.x264-GRP', 10, 'imported', 1700000009000, 1700000010000);
INSERT INTO pending_releases (id, series_id, title, release, reason, added, release_at)
VALUES (1, 20, 'Game.of.Thrones.S02.1080p-GRP', '{}', 'delay', 1700000023000, 1700000083000);

INSERT INTO issues (id, user_id, media_kind, tmdb_id, title, issue_type, message, resolved_by, created_at, updated_at)
VALUES (1, 2, 'movie', 603, 'The Matrix', 'audio', 'out of sync', 1, 1700000012000, 1700000012000);
INSERT INTO issue_comments (id, issue_id, user_id, message, created_at)
VALUES (1, 1, 3, 'same here', 1700000013000);
INSERT INTO user_notification_settings (user_id, settings, updated_at) VALUES (2, '{}', 1700000014000);
"#;

#[tokio::test]
async fn upgrade_from_0007_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let old_migrations = dir.path().join("migrations");
    std::fs::create_dir(&old_migrations).unwrap();
    copy_migrations_upto(&old_migrations, 7);
    let db_path = dir.path().join("reelix.db");

    // 1. A populated database at version 0007.
    let pool = open_pool(&db_path).await;
    sqlx::migrate::Migrator::new(old_migrations.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    exec(&pool, SEED_0007).await;

    let tables = [
        "users",
        "root_folders",
        "requests",
        "movies",
        "series",
        "quality_profiles",
        "history",
        "blocklist",
        "tracked_downloads",
        "pending_releases",
        "issues",
        "issue_comments",
    ];
    let mut before = Vec::new();
    for t in tables {
        let cols = columns(&pool, t).await;
        let rows = snapshot(&pool, t, &cols).await;
        assert!(!rows.is_empty(), "{t} has seed rows");
        before.push((t, cols, rows));
    }
    let requests_fks = foreign_keys(&pool, "requests").await;
    let requests_indexes = index_names(&pool, "requests").await;
    let roots_indexes = index_names(&pool, "root_folders").await;
    let requests_seq = seq(&pool, "requests").await;
    let roots_seq = seq(&pool, "root_folders").await;
    assert_eq!((requests_seq, roots_seq), (Some(6), Some(4)));
    pool.close().await;

    // 2. Upgrade with the embedded migrator (0008 onwards), twice to check it is idempotent.
    let url = format!("sqlite://{}", db_path.display());
    Db::connect(&url).await.unwrap().pool().close().await;
    let db = Db::connect(&url).await.unwrap();
    let pool = db.pool();

    let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .unwrap();
    let latest = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../migrations/sqlite"))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "sql"))
        .count() as i64;
    assert_eq!(versions, (1..=latest).collect::<Vec<i64>>());

    // Every old column of every table keeps every value.
    for (t, cols, rows) in &before {
        let now_cols = columns(pool, t).await;
        for c in cols {
            assert!(now_cols.contains(c), "{t}.{c} survived");
        }
        assert_eq!(&snapshot(pool, t, cols).await, rows, "{t} rows unchanged");
    }

    // New columns get their defaults on old rows.
    let row = sqlx::query(
        "SELECT foreign_id, subtitle, book_formats, book_id, album_id FROM requests WHERE id = 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(row.get::<Option<String>, _>("foreign_id"), None);
    assert_eq!(row.get::<Option<String>, _>("subtitle"), None);
    assert_eq!(row.get::<String, _>("book_formats"), "[]");
    assert_eq!(row.get::<Option<i64>, _>("book_id"), None);
    let kinds: Vec<String> = sqlx::query_scalar("SELECT DISTINCT media_kind FROM quality_profiles")
        .fetch_all(pool)
        .await
        .unwrap();
    assert_eq!(kinds, ["video"]);

    // Constraints, indexes and foreign keys of the rebuilt tables.
    assert_eq!(foreign_keys(pool, "requests").await, requests_fks);
    assert!(foreign_keys(pool, "root_folders").await.is_empty());
    let mut want = requests_indexes.clone();
    want.push("idx_requests_foreign".into());
    want.sort();
    assert_eq!(index_names(pool, "requests").await, want);
    assert_eq!(index_names(pool, "root_folders").await, roots_indexes);
    let fk_violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(pool)
        .await
        .unwrap();
    assert!(fk_violations.is_empty(), "foreign_key_check is empty");
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(integrity, "ok");

    // AUTOINCREMENT counters carried over: deleted ids are not reused.
    assert_eq!(seq(pool, "requests").await, requests_seq);
    assert_eq!(seq(pool, "root_folders").await, roots_seq);
    let id = sqlx::query(
        "INSERT INTO requests (user_id, media_kind, tmdb_id, title, created_at, updated_at)
         VALUES (2, 'movie', 13, 'Forrest Gump', 1, 1)",
    )
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    assert_eq!(id, 7);
    let id = sqlx::query("INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES ('/data/books', 'ebook', 1, 1)")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    assert_eq!(id, 5);

    // Widened kinds are accepted; old constraints still hold.
    let ok = |sql: &'static str| async move { sqlx::query(sql).execute(pool).await.is_ok() };
    assert!(ok("INSERT INTO requests (user_id, media_kind, foreign_id, title, book_formats, created_at, updated_at)
                VALUES (2, 'book', 'OL45883W', 'Dune', '[\"ebook\"]', 1, 1)").await);
    assert!(
        ok(
            "INSERT INTO requests (user_id, media_kind, foreign_id, title, created_at, updated_at)
                VALUES (2, 'music', 'f5093c06-23e3-404f-aeaa-40f72885ee3a', 'OK Computer', 1, 1)"
        )
        .await
    );
    assert!(
        !ok(
            "INSERT INTO requests (user_id, media_kind, title, created_at, updated_at)
                 VALUES (2, 'movie', 'No TMDB id', 1, 1)"
        )
        .await
    );
    assert!(
        !ok(
            "INSERT INTO requests (user_id, media_kind, title, created_at, updated_at)
                 VALUES (2, 'book', 'No work id', 1, 1)"
        )
        .await
    );
    assert!(
        !ok(
            "INSERT INTO requests (user_id, media_kind, tmdb_id, title, status, created_at, updated_at)
                 VALUES (2, 'movie', 1, 'Bad status', 'bogus', 1, 1)"
        )
        .await
    );
    assert!(
        !ok(
            "INSERT INTO requests (user_id, media_kind, tmdb_id, title, created_at, updated_at)
                 VALUES (99, 'movie', 1, 'No such user', 1, 1)"
        )
        .await
    );
    assert!(!ok("INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES ('/data/movies', 'music', 1, 1)").await);
    assert!(
        !ok(
            "INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES ('/x', 'book', 1, 1)"
        )
        .await
    );
    assert!(ok("INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES ('/data/music', 'music', 1, 1)").await);

    // ON DELETE actions still fire: SET NULL for modified_by, CASCADE for the owner.
    assert!(ok("DELETE FROM users WHERE id = 1").await);
    let modified: Option<i64> = sqlx::query_scalar("SELECT modified_by FROM requests WHERE id = 1")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(modified, None);
    assert!(ok("DELETE FROM users WHERE id = 3").await);
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE user_id = 3")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    let fk_violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(pool)
        .await
        .unwrap();
    assert!(fk_violations.is_empty());
}

#[tokio::test]
async fn fresh_database_has_media_tables() {
    let db = Db::in_memory().await.unwrap();
    for t in [
        "authors",
        "books",
        "book_authors",
        "book_editions",
        "book_series",
        "book_series_links",
        "book_files",
        "metadata_profiles",
        "artists",
        "albums",
        "album_releases",
        "track_files",
        "tracks",
        "music_list_exclusions",
        "metadata_cache",
    ] {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(t)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(n, 1, "table {t}");
    }
    let standard: String = sqlx::query_scalar("SELECT name FROM metadata_profiles")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(standard, "Standard");
}

/// Databases created between 0010 and the book/music defaults claimed the seed markers without
/// inserting profiles and wrote limitless placeholder definitions; 0011 lets the defaults seed.
#[tokio::test]
async fn reseeds_content_qualities_left_empty_by_early_builds() {
    use reelix_parser::model::{Quality, QualityFamily};

    let dir = tempfile::tempdir().unwrap();
    let old_migrations = dir.path().join("migrations");
    std::fs::create_dir(&old_migrations).unwrap();
    copy_migrations_upto(&old_migrations, 10);
    let db_path = dir.path().join("reelix.db");
    let pool = open_pool(&db_path).await;
    sqlx::migrate::Migrator::new(old_migrations.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    let mut stale = String::from(
        "INSERT INTO settings (key, value, updated_at) VALUES
            ('seeded.quality_profiles', 'true', 1),
            ('seeded.quality_profiles.ebook', 'true', 1),
            ('seeded.quality_profiles.audiobook', 'true', 1),
            ('seeded.quality_profiles.music', 'true', 1);",
    );
    for q in Quality::EVERY
        .iter()
        .filter(|q| q.family() != QualityFamily::Video)
    {
        stale.push_str(&format!(
            "INSERT INTO quality_definitions (quality_id, title, min_size, preferred_size, max_size, updated_at)
             VALUES ({}, 'placeholder', 0, NULL, NULL, 1);",
            q.id()
        ));
    }
    // A video definition with no limits is the user's choice and must survive.
    stale.push_str(&format!(
        "INSERT INTO quality_definitions (quality_id, title, min_size, preferred_size, max_size, updated_at)
         VALUES ({}, 'Mine', 0, NULL, NULL, 1);",
        Quality::Bluray1080p.id()
    ));
    exec(&pool, &stale).await;
    pool.close().await;

    let db = Db::connect(&format!("sqlite://{}", db_path.display()))
        .await
        .unwrap();
    for f in [
        QualityFamily::Ebook,
        QualityFamily::Audiobook,
        QualityFamily::Music,
    ] {
        assert!(
            !db.quality_profiles().list_family(f).await.unwrap().is_empty(),
            "{f:?} profiles"
        );
        let defs = db.quality_definitions().list_family(f).await.unwrap();
        assert!(defs.iter().all(|d| d.title != "placeholder"), "{f:?} definitions");
    }
    let video = db.quality_definitions().list().await.unwrap();
    let mine = video.iter().find(|d| d.quality == Quality::Bluray1080p).unwrap();
    assert_eq!((mine.title.as_str(), mine.max_size), ("Mine", None));
}
