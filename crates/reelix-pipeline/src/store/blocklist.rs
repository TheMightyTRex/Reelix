use chrono::{DateTime, Utc};
use reelix_core::Protocol;
use reelix_db::{Db, Result, from_ms, now_ms};
use reelix_parser::{Language, QualityModel};

use super::{ContentRef, from_json, json, parse_protocol, protocol_str};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewBlocklist {
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_ids: Vec<i64>,
    pub source_title: String,
    pub quality: Option<QualityModel>,
    pub languages: Vec<Language>,
    pub protocol: Option<Protocol>,
    pub indexer: Option<String>,
    pub size: Option<u64>,
    pub publish_date: Option<DateTime<Utc>>,
    pub torrent_info_hash: Option<String>,
    pub message: Option<String>,
    /// Book (format) / album.
    pub content: ContentRef,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlocklistEntry {
    pub id: i64,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_ids: Vec<i64>,
    pub source_title: String,
    pub quality: Option<QualityModel>,
    pub languages: Vec<Language>,
    pub protocol: Option<Protocol>,
    pub indexer: Option<String>,
    pub size: Option<u64>,
    pub publish_date: Option<DateTime<Utc>>,
    pub torrent_info_hash: Option<String>,
    pub message: Option<String>,
    pub date: DateTime<Utc>,
    pub content: ContentRef,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    episode_ids: String,
    source_title: String,
    quality: Option<String>,
    languages: String,
    protocol: Option<String>,
    indexer: Option<String>,
    size: Option<i64>,
    publish_date: Option<i64>,
    torrent_info_hash: Option<String>,
    message: Option<String>,
    date: i64,
    book_id: Option<i64>,
    book_format: Option<String>,
    album_id: Option<i64>,
    radio_episode_id: Option<i64>,
}

impl From<Row> for BlocklistEntry {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            movie_id: r.movie_id,
            series_id: r.series_id,
            episode_ids: from_json(&r.episode_ids),
            source_title: r.source_title,
            quality: r.quality.and_then(|q| serde_json::from_str(&q).ok()),
            languages: from_json(&r.languages),
            protocol: r.protocol.as_deref().and_then(parse_protocol),
            indexer: r.indexer,
            size: r.size.map(|s| s as u64),
            publish_date: r.publish_date.map(from_ms),
            torrent_info_hash: r.torrent_info_hash,
            message: r.message,
            date: from_ms(r.date),
            content: ContentRef::from_columns(
                r.book_id,
                r.book_format.as_deref(),
                r.album_id,
                r.radio_episode_id,
            ),
        }
    }
}

const COLS: &str = "id, movie_id, series_id, episode_ids, source_title, quality, languages, protocol, \
    indexer, size, publish_date, torrent_info_hash, message, date, book_id, book_format, album_id, \
    radio_episode_id";

pub async fn insert(db: &Db, b: &NewBlocklist) -> Result<i64> {
    let id = sqlx::query_scalar(
        "INSERT INTO blocklist (movie_id, series_id, episode_ids, source_title, quality, languages, protocol, \
         indexer, size, publish_date, torrent_info_hash, message, date, book_id, book_format, album_id, \
         radio_episode_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(b.movie_id)
    .bind(b.series_id)
    .bind(json(&b.episode_ids))
    .bind(&b.source_title)
    .bind(b.quality.as_ref().map(json))
    .bind(json(&b.languages))
    .bind(b.protocol.map(protocol_str))
    .bind(&b.indexer)
    .bind(b.size.map(|s| s as i64))
    .bind(b.publish_date.map(|d| d.timestamp_millis()))
    .bind(b.torrent_info_hash.as_deref().map(str::to_ascii_lowercase))
    .bind(&b.message)
    .bind(now_ms())
    .bind(b.content.book_id)
    .bind(b.content.format_str())
    .bind(b.content.album_id)
    .bind(b.content.radio_episode_id)
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

/// Newest first, with the total count.
pub async fn list(db: &Db, offset: i64, limit: i64) -> Result<(Vec<BlocklistEntry>, i64)> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blocklist")
        .fetch_one(db.pool())
        .await?;
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM blocklist ORDER BY date DESC, id DESC LIMIT ? OFFSET ?"
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(db.pool())
    .await?;
    Ok((rows.into_iter().map(Into::into).collect(), total))
}

/// Entries for one movie or series (used by the decision engine's blocklist check).
pub async fn for_media(
    db: &Db,
    movie_id: Option<i64>,
    series_id: Option<i64>,
) -> Result<Vec<BlocklistEntry>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM blocklist WHERE (?1 IS NOT NULL AND movie_id = ?1) OR (?2 IS NOT NULL AND series_id = ?2)"
    ))
    .bind(movie_id)
    .bind(series_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Entries for one book format (a book's entries without a format count for every format), one
/// album or one radio episode.
pub async fn for_content(db: &Db, c: &ContentRef) -> Result<Vec<BlocklistEntry>> {
    if c.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM blocklist WHERE (?1 IS NOT NULL AND book_id = ?1 AND (book_format IS NULL OR ?2 IS NULL \
         OR book_format = ?2)) OR (?3 IS NOT NULL AND album_id = ?3) \
         OR (?4 IS NOT NULL AND radio_episode_id = ?4)"
    ))
    .bind(c.book_id)
    .bind(c.format_str())
    .bind(c.album_id)
    .bind(c.radio_episode_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Whether a release is blocklisted: same info hash, or same title (case-insensitive) for the media.
pub fn is_blocked(entries: &[BlocklistEntry], title: &str, info_hash: Option<&str>) -> bool {
    entries.iter().any(|e| {
        info_hash.is_some_and(|h| {
            e.torrent_info_hash
                .as_deref()
                .is_some_and(|b| b.eq_ignore_ascii_case(h))
        }) || e.source_title.eq_ignore_ascii_case(title)
    })
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    Ok(sqlx::query("DELETE FROM blocklist WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?
        .rows_affected()
        > 0)
}

pub async fn delete_many(db: &Db, ids: &[i64]) -> Result<u64> {
    let mut n = 0;
    for id in ids {
        n += u64::from(delete(db, *id).await?);
    }
    Ok(n)
}

pub async fn clear(db: &Db) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM blocklist")
        .execute(db.pool())
        .await?
        .rows_affected())
}
