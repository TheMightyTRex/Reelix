//! `radio_episode_files` rows ⇄ [`RadioEpisodeFileDto`].

use reelix_api_types::radio::{RadioEpisodeFileDto, RadioLibraryShowDto};
use reelix_db::{Db, from_ms, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::placeholders;
use crate::convert::{from_ms_opt, ms, u32_opt};
use crate::error::{RadioError, Result};
use crate::library::NewEpisodeFile;

const COLS: &str = "f.id, f.abs_item_id, f.abs_episode_id, f.item_title, f.title, f.season, f.episode, f.guid, \
    f.enclosure_url, f.published_at, f.duration_secs, f.path, f.size, f.seen_at";

fn map(r: &SqliteRow) -> Result<RadioEpisodeFileDto> {
    Ok(RadioEpisodeFileDto {
        id: r.try_get("id")?,
        abs_item_id: r.try_get("abs_item_id")?,
        abs_episode_id: r.try_get("abs_episode_id")?,
        item_title: r.try_get("item_title")?,
        title: r.try_get("title")?,
        season: u32_opt(r.try_get("season")?),
        episode: u32_opt(r.try_get("episode")?),
        guid: r.try_get("guid")?,
        enclosure_url: r.try_get("enclosure_url")?,
        published_at: from_ms_opt(r.try_get("published_at")?),
        duration_secs: u32_opt(r.try_get("duration_secs")?),
        path: r.try_get("path")?,
        size: r.try_get::<Option<i64>, _>("size")?.map(|s| s.max(0) as u64),
        seen_at: from_ms(r.try_get("seen_at")?),
    })
}

pub async fn get(db: &Db, id: i64) -> Result<Option<RadioEpisodeFileDto>> {
    let row = sqlx::query(&format!(
        "SELECT {COLS} FROM radio_episode_files f WHERE f.id = ?"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    row.as_ref().map(map).transpose()
}

pub async fn require(db: &Db, id: i64) -> Result<RadioEpisodeFileDto> {
    get(db, id)
        .await?
        .ok_or_else(|| RadioError::NotFound(format!("radio episode file {id} not found")))
}

/// Every file (for matching), by item then publication.
pub async fn all(db: &Db) -> Result<Vec<RadioEpisodeFileDto>> {
    sqlx::query(&format!(
        "SELECT {COLS} FROM radio_episode_files f ORDER BY f.abs_item_id, f.published_at, f.id"
    ))
    .fetch_all(db.pool())
    .await?
    .iter()
    .map(map)
    .collect()
}

/// Files no episode is linked to.
pub async fn unmatched(db: &Db) -> Result<Vec<RadioEpisodeFileDto>> {
    sqlx::query(&format!(
        "SELECT {COLS} FROM radio_episode_files f \
         WHERE NOT EXISTS (SELECT 1 FROM radio_episodes e WHERE e.file_id = f.id) \
         ORDER BY f.item_title COLLATE NOCASE, f.published_at, f.id"
    ))
    .fetch_all(db.pool())
    .await?
    .iter()
    .map(map)
    .collect()
}

/// Library podcast items that no show maps to (`radio_shows.abs_item_id`) and none of whose
/// files is linked to an episode.
pub async fn unmatched_items(db: &Db) -> Result<Vec<RadioLibraryShowDto>> {
    let rows = sqlx::query(
        "SELECT f.abs_item_id, MAX(f.item_title) AS item_title, COUNT(*) AS n FROM radio_episode_files f \
         WHERE NOT EXISTS (SELECT 1 FROM radio_shows s WHERE s.abs_item_id = f.abs_item_id) \
         GROUP BY f.abs_item_id \
         HAVING NOT EXISTS (SELECT 1 FROM radio_episodes e JOIN radio_episode_files g ON g.id = e.file_id \
                            WHERE g.abs_item_id = f.abs_item_id) \
         ORDER BY item_title COLLATE NOCASE, f.abs_item_id",
    )
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| {
            Ok(RadioLibraryShowDto {
                abs_item_id: r.try_get("abs_item_id")?,
                title: r.try_get("item_title")?,
                file_count: r.try_get::<i64, _>("n")?.max(0) as u32,
            })
        })
        .collect()
}

/// Shows whose follow choice is not `none`.
pub async fn followed_shows(db: &Db) -> Result<u32> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM radio_shows WHERE follow <> 'none'")
        .fetch_one(db.pool())
        .await?;
    Ok(n.max(0) as u32)
}

/// Inserts or updates a file by (`abs_item_id`, `abs_episode_id`), stamping `seen_at`.
/// Returns the id.
pub async fn upsert(db: &Db, f: &NewEpisodeFile, seen_at: i64) -> Result<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO radio_episode_files (abs_item_id, abs_episode_id, item_title, title, season, episode, guid, \
            enclosure_url, published_at, duration_secs, path, size, seen_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (abs_item_id, abs_episode_id) DO UPDATE SET item_title = excluded.item_title, \
            title = excluded.title, season = excluded.season, episode = excluded.episode, guid = excluded.guid, \
            enclosure_url = excluded.enclosure_url, published_at = excluded.published_at, \
            duration_secs = excluded.duration_secs, path = excluded.path, size = excluded.size, \
            seen_at = excluded.seen_at \
         RETURNING id",
    )
    .bind(&f.abs_item_id)
    .bind(&f.abs_episode_id)
    .bind(&f.item_title)
    .bind(&f.title)
    .bind(f.season.map(i64::from))
    .bind(f.episode.map(i64::from))
    .bind(&f.guid)
    .bind(&f.enclosure_url)
    .bind(ms(f.published_at))
    .bind(f.duration_secs.map(i64::from))
    .bind(&f.path)
    .bind(f.size.map(|s| s.min(i64::MAX as u64) as i64))
    .bind(seen_at)
    .bind(now_ms())
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

/// Ids of files last seen before `seen_before`.
pub async fn unseen(db: &Db, seen_before: i64) -> Result<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM radio_episode_files WHERE seen_at < ? ORDER BY id")
            .bind(seen_before)
            .fetch_all(db.pool())
            .await?,
    )
}

pub async fn delete(db: &Db, ids: &[i64]) -> Result<u64> {
    let sql = format!(
        "DELETE FROM radio_episode_files WHERE id IN ({})",
        placeholders(ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id);
    }
    Ok(q.execute(db.pool()).await?.rows_affected())
}
