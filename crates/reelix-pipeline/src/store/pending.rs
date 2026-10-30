use chrono::{DateTime, Utc};
use reelix_db::{Db, Result, from_ms, now_ms};

use super::{ContentRef, GrabbedRelease, from_json, json};

#[derive(Debug, Clone, PartialEq)]
pub struct PendingRelease {
    pub id: i64,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_ids: Vec<i64>,
    pub title: String,
    pub release: GrabbedRelease,
    /// `delay` or `preferred_protocol`.
    pub reason: String,
    pub added: DateTime<Utc>,
    pub release_at: DateTime<Utc>,
    /// Book (format) / album.
    pub content: ContentRef,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    episode_ids: String,
    title: String,
    release: String,
    reason: String,
    added: i64,
    release_at: i64,
    book_id: Option<i64>,
    book_format: Option<String>,
    album_id: Option<i64>,
}

impl TryFrom<Row> for PendingRelease {
    type Error = serde_json::Error;
    fn try_from(r: Row) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            id: r.id,
            movie_id: r.movie_id,
            series_id: r.series_id,
            episode_ids: from_json(&r.episode_ids),
            title: r.title,
            release: serde_json::from_str(&r.release)?,
            reason: r.reason,
            added: from_ms(r.added),
            release_at: from_ms(r.release_at),
            content: ContentRef::from_columns(r.book_id, r.book_format.as_deref(), r.album_id),
        })
    }
}

const COLS: &str = "id, movie_id, series_id, episode_ids, title, release, reason, added, release_at, book_id, book_format, album_id";

#[allow(clippy::too_many_arguments)]
pub async fn insert(
    db: &Db,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    episode_ids: &[i64],
    release: &GrabbedRelease,
    reason: &str,
    release_at_ms: i64,
) -> Result<i64> {
    let id = sqlx::query_scalar(
        "INSERT INTO pending_releases (movie_id, series_id, episode_ids, title, release, reason, added, release_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(movie_id)
    .bind(series_id)
    .bind(json(&episode_ids))
    .bind(&release.title)
    .bind(json(release))
    .bind(reason)
    .bind(now_ms())
    .bind(release_at_ms)
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

/// Inserts a delayed book / album release.
pub async fn insert_content(
    db: &Db,
    content: &ContentRef,
    release: &GrabbedRelease,
    reason: &str,
    release_at_ms: i64,
) -> Result<i64> {
    let id = sqlx::query_scalar(
        "INSERT INTO pending_releases (movie_id, series_id, episode_ids, title, release, reason, added, release_at, \
         book_id, book_format, album_id) VALUES (NULL, NULL, '[]', ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&release.title)
    .bind(json(release))
    .bind(reason)
    .bind(now_ms())
    .bind(release_at_ms)
    .bind(content.book_id)
    .bind(content.format_str())
    .bind(content.album_id)
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

async fn query(db: &Db, sql: &str, bind: Option<i64>) -> Result<Vec<PendingRelease>> {
    let mut q = sqlx::query_as::<_, Row>(sql);
    if let Some(v) = bind {
        q = q.bind(v);
    }
    let rows = q.fetch_all(db.pool()).await?;
    // Rows that no longer deserialize (format change) are skipped rather than failing the queue.
    Ok(rows.into_iter().filter_map(|r| r.try_into().ok()).collect())
}

pub async fn all(db: &Db) -> Result<Vec<PendingRelease>> {
    query(
        db,
        &format!("SELECT {COLS} FROM pending_releases ORDER BY release_at"),
        None,
    )
    .await
}

/// Releases whose delay has elapsed.
pub async fn due(db: &Db, now_ms: i64) -> Result<Vec<PendingRelease>> {
    query(
        db,
        &format!("SELECT {COLS} FROM pending_releases WHERE release_at <= ? ORDER BY release_at"),
        Some(now_ms),
    )
    .await
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    Ok(sqlx::query("DELETE FROM pending_releases WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?
        .rows_affected()
        > 0)
}

/// Drops pending releases for media (e.g. after a better release was grabbed).
pub async fn delete_for_media(db: &Db, movie_id: Option<i64>, series_id: Option<i64>) -> Result<u64> {
    Ok(sqlx::query(
        "DELETE FROM pending_releases WHERE (?1 IS NOT NULL AND movie_id = ?1) OR (?2 IS NOT NULL AND series_id = ?2)",
    )
    .bind(movie_id)
    .bind(series_id)
    .execute(db.pool())
    .await?
    .rows_affected())
}
