//! `authors` rows ⇄ [`AuthorDto`] (with statistics).

use std::collections::HashMap;

use reelix_api_types::books::{AuthorDto, AuthorStatisticsDto, NewBookMonitor};
use reelix_db::{Db, from_ms, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{clean, from_json, json, placeholders};
use crate::error::Result;

const COLS: &str = "id, openlibrary_id, name, sort_name, overview, birth_date, death_date, images, links, \
    monitored, monitor_new_items, quality_profile_id, root_folder_id, path, audio_quality_profile_id, \
    audio_root_folder_id, audio_path, tags, last_refreshed, created_at, custom";

fn new_items_str(m: NewBookMonitor) -> &'static str {
    match m {
        NewBookMonitor::All => "all",
        NewBookMonitor::None => "none",
    }
}

fn map(r: &SqliteRow) -> Result<AuthorDto> {
    let new_items: String = r.try_get("monitor_new_items")?;
    Ok(AuthorDto {
        id: r.try_get("id")?,
        openlibrary_id: r.try_get("openlibrary_id")?,
        name: r.try_get("name")?,
        sort_name: r.try_get("sort_name")?,
        overview: r.try_get("overview")?,
        birth_date: r.try_get("birth_date")?,
        death_date: r.try_get("death_date")?,
        images: from_json(r.try_get("images")?),
        links: from_json(r.try_get("links")?),
        monitored: r.try_get("monitored")?,
        monitor_new_items: if new_items == "none" {
            NewBookMonitor::None
        } else {
            NewBookMonitor::All
        },
        quality_profile_id: r.try_get("quality_profile_id")?,
        root_folder_id: r.try_get("root_folder_id")?,
        path: r.try_get("path")?,
        audio_quality_profile_id: r.try_get("audio_quality_profile_id")?,
        audio_root_folder_id: r.try_get("audio_root_folder_id")?,
        audio_path: r.try_get("audio_path")?,
        tags: from_json(r.try_get("tags")?),
        statistics: AuthorStatisticsDto::default(),
        added: from_ms(r.try_get("created_at")?),
        last_refreshed: r.try_get::<Option<i64>, _>("last_refreshed")?.map(from_ms),
        custom: r.try_get("custom")?,
    })
}

/// Authors (all, or `ids`) ordered by sort name, with statistics.
pub async fn load(db: &Db, ids: Option<&[i64]>) -> Result<Vec<AuthorDto>> {
    let sql = match ids {
        Some(ids) => format!(
            "SELECT {COLS} FROM authors WHERE id IN ({}) ORDER BY sort_name",
            placeholders(ids.len())
        ),
        None => format!("SELECT {COLS} FROM authors ORDER BY sort_name"),
    };
    let mut q = sqlx::query(&sql);
    for id in ids.unwrap_or_default() {
        q = q.bind(id);
    }
    let mut out: Vec<AuthorDto> = q
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map)
        .collect::<Result<_>>()?;
    if out.is_empty() {
        return Ok(out);
    }
    let mut stats = statistics(db).await?;
    for a in &mut out {
        a.statistics = stats.remove(&a.id).unwrap_or_default();
    }
    Ok(out)
}

pub async fn get(db: &Db, id: i64) -> Result<Option<AuthorDto>> {
    Ok(load(db, Some(&[id])).await?.into_iter().next())
}

pub async fn id_by_olid(db: &Db, olid: &str) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM authors WHERE openlibrary_id = ?")
            .bind(olid)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// Open Library id → library author id.
pub async fn olid_index(db: &Db) -> Result<HashMap<String, i64>> {
    let rows = sqlx::query("SELECT id, openlibrary_id FROM authors")
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| Ok((r.try_get("openlibrary_id")?, r.try_get("id")?)))
        .collect()
}

pub async fn insert(db: &Db, a: &AuthorDto, added_by_request_id: Option<i64>) -> Result<i64> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO authors (openlibrary_id, name, sort_name, clean_name, overview, birth_date, death_date, \
         images, links, monitored, monitor_new_items, quality_profile_id, root_folder_id, path, \
         audio_quality_profile_id, audio_root_folder_id, audio_path, tags, added_by_request_id, \
         last_refreshed, custom, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&a.openlibrary_id)
    .bind(&a.name)
    .bind(&a.sort_name)
    .bind(clean(&a.name))
    .bind(&a.overview)
    .bind(&a.birth_date)
    .bind(&a.death_date)
    .bind(json(&a.images))
    .bind(json(&a.links))
    .bind(a.monitored)
    .bind(new_items_str(a.monitor_new_items))
    .bind(a.quality_profile_id)
    .bind(a.root_folder_id)
    .bind(&a.path)
    .bind(a.audio_quality_profile_id)
    .bind(a.audio_root_folder_id)
    .bind(&a.audio_path)
    .bind(json(&a.tags))
    .bind(added_by_request_id)
    .bind(a.last_refreshed.map(|t| t.timestamp_millis()))
    .bind(a.custom)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(res.last_insert_rowid())
}

/// Writes every field of `a` (metadata and user settings).
pub async fn save(db: &Db, a: &AuthorDto) -> Result<()> {
    sqlx::query(
        "UPDATE authors SET name = ?, sort_name = ?, clean_name = ?, overview = ?, birth_date = ?, \
         death_date = ?, images = ?, links = ?, monitored = ?, monitor_new_items = ?, quality_profile_id = ?, \
         root_folder_id = ?, path = ?, audio_quality_profile_id = ?, audio_root_folder_id = ?, audio_path = ?, \
         tags = ?, last_refreshed = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&a.name)
    .bind(&a.sort_name)
    .bind(clean(&a.name))
    .bind(&a.overview)
    .bind(&a.birth_date)
    .bind(&a.death_date)
    .bind(json(&a.images))
    .bind(json(&a.links))
    .bind(a.monitored)
    .bind(new_items_str(a.monitor_new_items))
    .bind(a.quality_profile_id)
    .bind(a.root_folder_id)
    .bind(&a.path)
    .bind(a.audio_quality_profile_id)
    .bind(a.audio_root_folder_id)
    .bind(&a.audio_path)
    .bind(json(&a.tags))
    .bind(a.last_refreshed.map(|t| t.timestamp_millis()))
    .bind(now_ms())
    .bind(a.id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM authors WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Authors using a root folder (either format).
pub async fn count_using_root(db: &Db, root_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM authors WHERE root_folder_id = ? OR audio_root_folder_id = ?",
    )
    .bind(root_id)
    .bind(root_id)
    .fetch_one(db.pool())
    .await?)
}

async fn statistics(db: &Db) -> Result<HashMap<i64, AuthorStatisticsDto>> {
    let mut out: HashMap<i64, AuthorStatisticsDto> = HashMap::new();
    let rows = sqlx::query(
        "SELECT author_id, COUNT(*) AS n, \
         SUM(CASE WHEN monitored_ebook = 1 OR monitored_audiobook = 1 THEN 1 ELSE 0 END) AS m \
         FROM books GROUP BY author_id",
    )
    .fetch_all(db.pool())
    .await?;
    for r in rows {
        let s = out.entry(r.try_get("author_id")?).or_default();
        s.book_count = r.try_get::<i64, _>("n")? as u32;
        s.monitored_count = r.try_get::<Option<i64>, _>("m")?.unwrap_or(0) as u32;
    }
    let rows = sqlx::query(
        "SELECT b.author_id, f.format, COUNT(*) AS n, SUM(f.size) AS s \
         FROM book_files f JOIN books b ON b.id = f.book_id GROUP BY b.author_id, f.format",
    )
    .fetch_all(db.pool())
    .await?;
    for r in rows {
        let s = out.entry(r.try_get("author_id")?).or_default();
        let n = r.try_get::<i64, _>("n")? as u32;
        if r.try_get::<String, _>("format")? == "audiobook" {
            s.audiobook_file_count = n;
        } else {
            s.ebook_file_count = n;
        }
        s.size_on_disk += r.try_get::<Option<i64>, _>("s")?.unwrap_or(0).max(0) as u64;
    }
    Ok(out)
}
