//! `metadata_profiles` rows ⇄ [`MetadataProfileDto`].

use reelix_api_types::music::MetadataProfileDto;
use reelix_db::{Db, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::convert::{from_json, json};
use crate::error::{MusicError, Result};

fn map(r: &SqliteRow) -> Result<MetadataProfileDto> {
    Ok(MetadataProfileDto {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        primary_types: from_json(r.try_get("primary_types")?),
        secondary_types: from_json(r.try_get("secondary_types")?),
        release_statuses: from_json(r.try_get("release_statuses")?),
    })
}

const SELECT: &str =
    "SELECT id, name, primary_types, secondary_types, release_statuses FROM metadata_profiles";

pub async fn list(db: &Db) -> Result<Vec<MetadataProfileDto>> {
    let rows = sqlx::query(&format!("{SELECT} ORDER BY id"))
        .fetch_all(db.pool())
        .await?;
    rows.iter().map(map).collect()
}

pub async fn get(db: &Db, id: i64) -> Result<Option<MetadataProfileDto>> {
    let row = sqlx::query(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    row.as_ref().map(map).transpose()
}

fn conflict(e: sqlx::Error, name: &str) -> MusicError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            MusicError::Conflict(format!("a metadata profile named {name:?} already exists"))
        }
        _ => e.into(),
    }
}

pub async fn insert(db: &Db, p: &MetadataProfileDto) -> Result<i64> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO metadata_profiles (name, primary_types, secondary_types, release_statuses, created_at,
            updated_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(p.name.trim())
    .bind(json(&p.primary_types))
    .bind(json(&p.secondary_types))
    .bind(json(&p.release_statuses))
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| conflict(e, &p.name))?;
    Ok(res.last_insert_rowid())
}

pub async fn update(db: &Db, id: i64, p: &MetadataProfileDto) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE metadata_profiles SET name = ?, primary_types = ?, secondary_types = ?, release_statuses = ?,
            updated_at = ? WHERE id = ?",
    )
    .bind(p.name.trim())
    .bind(json(&p.primary_types))
    .bind(json(&p.secondary_types))
    .bind(json(&p.release_statuses))
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await
    .map_err(|e| conflict(e, &p.name))?;
    Ok(res.rows_affected() > 0)
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM metadata_profiles WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}
