//! Root folders, collections, tags and alternative titles.

use std::collections::HashMap;

use reelix_api_types::library::{CastDto, CollectionEdit, MinimumAvailability};
use reelix_core::MediaKind;
use reelix_db::{Db, from_ms, now_ms};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{kind_str, parse_kind, placeholders};
use crate::convert::{clean_title, enum_str, from_json, json, parse_enum};
use crate::error::{LibraryError, Result};

// ---------------- root folders ----------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootFolder {
    pub id: i64,
    pub path: String,
    pub media_kind: MediaKind,
}

/// Movie and series root folders (book/music roots live in the same table; see
/// [`crate::content_roots`]).
pub async fn root_folders(db: &Db) -> Result<Vec<RootFolder>> {
    let rows = sqlx::query(
        "SELECT id, path, media_kind FROM root_folders WHERE media_kind IN ('movie', 'series') ORDER BY path",
    )
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| {
            Ok(RootFolder {
                id: r.try_get("id")?,
                path: r.try_get("path")?,
                media_kind: parse_kind(r.try_get::<String, _>("media_kind")?.as_str()),
            })
        })
        .collect()
}

pub async fn root_folder(db: &Db, id: i64) -> Result<Option<RootFolder>> {
    Ok(root_folders(db).await?.into_iter().find(|r| r.id == id))
}

pub async fn insert_root_folder(db: &Db, path: &str, kind: MediaKind) -> Result<i64> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES (?, ?, ?, ?)",
    )
    .bind(path)
    .bind(kind_str(kind))
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            LibraryError::Conflict(format!("root folder {path:?} already exists"))
        }
        _ => e.into(),
    })?;
    Ok(res.last_insert_rowid())
}

pub async fn delete_root_folder(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM root_folders WHERE id = ? AND media_kind IN ('movie', 'series')")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Movies + series using the root folder.
pub async fn root_folder_usage(db: &Db, id: i64) -> Result<i64> {
    let n: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM movies WHERE root_folder_id = ?1) + (SELECT COUNT(*) FROM series WHERE root_folder_id = ?1)",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await?;
    Ok(n)
}

// ---------------- lookups by media id ----------------

/// Above this many ids, one read of the whole kind is cheaper than a long `IN (…)` list (and
/// stays under SQLite's bound-parameter limit).
const MAX_SCOPED_IDS: usize = 500;

fn use_ids(ids: Option<&[i64]>) -> Option<&[i64]> {
    ids.filter(|ids| ids.len() <= MAX_SCOPED_IDS)
}

/// `base`, narrowed to `ids` when they are few enough, then `order`.
fn scoped(base: &str, ids: Option<&[i64]>, order: &str) -> String {
    match use_ids(ids) {
        Some(ids) => format!("{base} AND media_id IN ({}) {order}", placeholders(ids.len())),
        None => format!("{base} {order}"),
    }
}

// ---------------- tags ----------------

/// Tags by media id, for every item of `kind` or only `ids`.
pub async fn tags_of(db: &Db, kind: MediaKind, ids: Option<&[i64]>) -> Result<HashMap<i64, Vec<i64>>> {
    let sql = scoped(
        "SELECT media_id, tag_id FROM media_tags WHERE media_kind = ?",
        ids,
        "ORDER BY tag_id",
    );
    let mut q = sqlx::query(&sql).bind(kind_str(kind));
    for id in use_ids(ids).unwrap_or_default() {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db.pool()).await?;
    let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
    for r in rows {
        out.entry(r.try_get("media_id")?)
            .or_default()
            .push(r.try_get("tag_id")?);
    }
    Ok(out)
}

pub async fn set_tags(db: &Db, kind: MediaKind, id: i64, tags: &[i64]) -> Result<()> {
    let mut tx = db.pool().begin().await?;
    sqlx::query("DELETE FROM media_tags WHERE media_kind = ? AND media_id = ?")
        .bind(kind_str(kind))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for t in tags {
        sqlx::query("INSERT OR IGNORE INTO media_tags (media_kind, media_id, tag_id) VALUES (?, ?, ?)")
            .bind(kind_str(kind))
            .bind(id)
            .bind(t)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------- alternative titles ----------------

/// Alternative titles by media id, for every item of `kind` or only `ids`.
pub async fn alt_titles_of(
    db: &Db,
    kind: MediaKind,
    ids: Option<&[i64]>,
) -> Result<HashMap<i64, Vec<String>>> {
    let sql = scoped(
        "SELECT media_id, title FROM alternative_titles WHERE media_kind = ?",
        ids,
        "ORDER BY id",
    );
    let mut q = sqlx::query(&sql).bind(kind_str(kind));
    for id in use_ids(ids).unwrap_or_default() {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db.pool()).await?;
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    for r in rows {
        out.entry(r.try_get("media_id")?)
            .or_default()
            .push(r.try_get("title")?);
    }
    Ok(out)
}

pub async fn set_alt_titles(
    db: &Db,
    kind: MediaKind,
    id: i64,
    titles: &[(String, Option<String>)],
) -> Result<()> {
    let mut tx = db.pool().begin().await?;
    sqlx::query("DELETE FROM alternative_titles WHERE media_kind = ? AND media_id = ?")
        .bind(kind_str(kind))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let mut seen = std::collections::HashSet::new();
    for (title, country) in titles {
        let clean = clean_title(title);
        if clean.is_empty() || !seen.insert(clean.clone()) {
            continue;
        }
        sqlx::query(
            "INSERT INTO alternative_titles (media_kind, media_id, title, clean_title, country) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(kind_str(kind))
        .bind(id)
        .bind(title)
        .bind(clean)
        .bind(country)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Removes tags and alternative titles of a deleted movie/series.
pub async fn delete_media_extras(db: &Db, kind: MediaKind, id: i64) -> Result<()> {
    for table in ["media_tags", "alternative_titles"] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE media_kind = ? AND media_id = ?"
        ))
        .bind(kind_str(kind))
        .bind(id)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

// ---------------- credits ----------------

pub async fn credits(db: &Db, kind: MediaKind, id: i64) -> Result<Vec<CastDto>> {
    let table = if kind == MediaKind::Movie {
        "movies"
    } else {
        "series"
    };
    let raw: Option<String> = sqlx::query_scalar(&format!("SELECT credits FROM {table} WHERE id = ?"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    Ok(from_json(raw))
}

pub async fn set_credits(db: &Db, kind: MediaKind, id: i64, cast: &[CastDto]) -> Result<()> {
    let table = if kind == MediaKind::Movie {
        "movies"
    } else {
        "series"
    };
    sqlx::query(&format!("UPDATE {table} SET credits = ? WHERE id = ?"))
        .bind(json(&cast))
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

// ---------------- collections ----------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CollectionPart {
    pub tmdb_id: u32,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub poster: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionRow {
    pub id: i64,
    pub tmdb_id: u32,
    pub title: String,
    pub overview: Option<String>,
    pub images: reelix_api_types::library::ImagesDto,
    pub parts: Vec<CollectionPart>,
    pub monitored: bool,
    pub quality_profile_id: i64,
    pub root_folder_id: i64,
    pub minimum_availability: MinimumAvailability,
    pub search_on_add: bool,
    pub tags: Vec<i64>,
    pub last_synced: Option<chrono::DateTime<chrono::Utc>>,
}

const COLLECTION_COLS: &str = "id, tmdb_id, title, overview, images, parts, monitored, quality_profile_id, root_folder_id, \
     minimum_availability, search_on_add, tags, last_synced";

fn map_collection(r: &sqlx::sqlite::SqliteRow) -> Result<CollectionRow> {
    Ok(CollectionRow {
        id: r.try_get("id")?,
        tmdb_id: r.try_get::<i64, _>("tmdb_id")? as u32,
        title: r.try_get("title")?,
        overview: r.try_get("overview")?,
        images: from_json(r.try_get("images")?),
        parts: from_json(r.try_get("parts")?),
        monitored: r.try_get("monitored")?,
        quality_profile_id: r.try_get("quality_profile_id")?,
        root_folder_id: r.try_get("root_folder_id")?,
        minimum_availability: parse_enum(&r.try_get::<String, _>("minimum_availability")?)
            .unwrap_or_default(),
        search_on_add: r.try_get("search_on_add")?,
        tags: from_json(r.try_get("tags")?),
        last_synced: r.try_get::<Option<i64>, _>("last_synced")?.map(from_ms),
    })
}

pub async fn collections(db: &Db, ids: Option<&[i64]>) -> Result<Vec<CollectionRow>> {
    let sql = match ids {
        Some(ids) => format!(
            "SELECT {COLLECTION_COLS} FROM collections WHERE id IN ({}) ORDER BY sort_title",
            placeholders(ids.len())
        ),
        None => format!("SELECT {COLLECTION_COLS} FROM collections ORDER BY sort_title"),
    };
    let mut q = sqlx::query(&sql);
    for id in ids.unwrap_or_default() {
        q = q.bind(id);
    }
    q.fetch_all(db.pool()).await?.iter().map(map_collection).collect()
}

pub async fn collection_by_tmdb(db: &Db, tmdb_id: u32) -> Result<Option<CollectionRow>> {
    let row = sqlx::query(&format!(
        "SELECT {COLLECTION_COLS} FROM collections WHERE tmdb_id = ?"
    ))
    .bind(tmdb_id as i64)
    .fetch_optional(db.pool())
    .await?;
    row.as_ref().map(map_collection).transpose()
}

/// Inserts a collection or refreshes its title/overview/images/parts; returns its id.
pub async fn upsert_collection(db: &Db, c: &CollectionRow) -> Result<i64> {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO collections (tmdb_id, title, sort_title, overview, images, parts, monitored, quality_profile_id,
             root_folder_id, minimum_availability, search_on_add, tags, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(tmdb_id) DO UPDATE SET title = excluded.title, sort_title = excluded.sort_title,
             overview = excluded.overview, images = excluded.images,
             parts = CASE WHEN excluded.parts = '[]' THEN collections.parts ELSE excluded.parts END,
             updated_at = excluded.updated_at",
    )
    .bind(c.tmdb_id as i64)
    .bind(&c.title)
    .bind(reelix_metadata::sort_title(&c.title))
    .bind(&c.overview)
    .bind(json(&c.images))
    .bind(json(&c.parts))
    .bind(c.monitored)
    .bind(c.quality_profile_id)
    .bind(c.root_folder_id)
    .bind(enum_str(&c.minimum_availability))
    .bind(c.search_on_add)
    .bind(json(&c.tags))
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM collections WHERE tmdb_id = ?")
        .bind(c.tmdb_id as i64)
        .fetch_one(db.pool())
        .await?;
    Ok(id)
}

pub async fn set_collection_synced(db: &Db, id: i64, parts: &[CollectionPart]) -> Result<()> {
    sqlx::query("UPDATE collections SET parts = ?, last_synced = ?, updated_at = ? WHERE id = ?")
        .bind(json(&parts))
        .bind(now_ms())
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn update_collection(db: &Db, id: i64, e: &CollectionEdit) -> Result<bool> {
    let Some(mut c) = collections(db, Some(&[id])).await?.pop() else {
        return Ok(false);
    };
    if let Some(v) = e.monitored {
        c.monitored = v;
    }
    if let Some(v) = e.quality_profile_id {
        c.quality_profile_id = v;
    }
    if let Some(v) = e.root_folder_id {
        c.root_folder_id = v;
    }
    if let Some(v) = e.minimum_availability {
        c.minimum_availability = v;
    }
    if let Some(v) = e.search_on_add {
        c.search_on_add = v;
    }
    if let Some(v) = &e.tags {
        c.tags = v.clone();
    }
    sqlx::query(
        "UPDATE collections SET monitored = ?, quality_profile_id = ?, root_folder_id = ?, minimum_availability = ?,
             search_on_add = ?, tags = ?, updated_at = ? WHERE id = ?",
    )
    .bind(c.monitored)
    .bind(c.quality_profile_id)
    .bind(c.root_folder_id)
    .bind(enum_str(&c.minimum_availability))
    .bind(c.search_on_add)
    .bind(json(&c.tags))
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(true)
}

/// Deletes collections no movie references any more (unless monitored).
pub async fn prune_collections(db: &Db) -> Result<u64> {
    let res = sqlx::query(
        "DELETE FROM collections WHERE monitored = 0 AND id NOT IN (SELECT collection_id FROM movies WHERE collection_id IS NOT NULL)",
    )
    .execute(db.pool())
    .await?;
    Ok(res.rows_affected())
}
