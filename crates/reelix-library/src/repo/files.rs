//! `movie_files` and `episode_files`.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use reelix_api_types::library::MediaFileDto;
use reelix_db::{Db, from_ms, now_ms};
use reelix_decision::QualityProfile;
use reelix_import::mediainfo::MediaInfo;
use reelix_parser::{Language, QualityModel};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::placeholders;
use crate::convert::{from_json, json, media_info_dto};
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTable {
    Movie,
    Episode,
}

impl FileTable {
    fn table(self) -> &'static str {
        match self {
            Self::Movie => "movie_files",
            Self::Episode => "episode_files",
        }
    }

    fn owner(self) -> &'static str {
        match self {
            Self::Movie => "movie_id",
            Self::Episode => "series_id",
        }
    }
}

/// A stored media file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    pub id: i64,
    /// Movie id or series id.
    pub owner_id: i64,
    /// Episode files only.
    pub season_number: Option<u32>,
    pub relative_path: String,
    pub size: u64,
    pub quality: QualityModel,
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub edition: Option<String>,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    pub media_info: Option<MediaInfo>,
    pub custom_format_ids: Vec<i64>,
    pub custom_format_score: i32,
    pub date_added: DateTime<Utc>,
}

impl FileRow {
    /// DTO with the cutoff computed against `profile` and format names resolved.
    pub fn to_dto(
        &self,
        media_path: &str,
        profile: Option<&QualityProfile>,
        format_names: &HashMap<i64, String>,
    ) -> MediaFileDto {
        MediaFileDto {
            id: self.id,
            relative_path: self.relative_path.clone(),
            path: Path::new(media_path)
                .join(&self.relative_path)
                .to_string_lossy()
                .into_owned(),
            size: self.size,
            quality: self.quality,
            quality_name: self.quality.quality.name().to_owned(),
            languages: self.languages.clone(),
            release_group: self.release_group.clone(),
            edition: self.edition.clone(),
            scene_name: self.scene_name.clone(),
            original_path: self.original_path.clone(),
            media_info: self.media_info.as_ref().map(media_info_dto),
            custom_format_ids: self.custom_format_ids.clone(),
            custom_formats: self
                .custom_format_ids
                .iter()
                .filter_map(|id| format_names.get(id).cloned())
                .collect(),
            custom_format_score: self.custom_format_score,
            cutoff_met: profile.is_some_and(|p| p.cutoff_met(self.quality.quality, self.custom_format_score)),
            date_added: self.date_added,
        }
    }
}

const COLS: &str = "relative_path, size, quality, languages, release_group, edition, scene_name, original_path, \
                    media_info, custom_format_ids, custom_format_score, date_added";

fn map(t: FileTable, r: &SqliteRow) -> Result<FileRow> {
    let quality: Option<String> = r.try_get("quality")?;
    let media_info: Option<String> = r.try_get("media_info")?;
    Ok(FileRow {
        id: r.try_get("id")?,
        owner_id: r.try_get(t.owner())?,
        season_number: match t {
            FileTable::Episode => Some(r.try_get::<i64, _>("season_number")? as u32),
            FileTable::Movie => None,
        },
        relative_path: r.try_get("relative_path")?,
        size: r.try_get::<i64, _>("size")?.max(0) as u64,
        quality: quality
            .and_then(|q| serde_json::from_str(&q).ok())
            .unwrap_or(QualityModel::new(reelix_parser::Quality::Unknown)),
        languages: from_json(r.try_get("languages")?),
        release_group: r.try_get("release_group")?,
        edition: r.try_get("edition")?,
        scene_name: r.try_get("scene_name")?,
        original_path: r.try_get("original_path")?,
        media_info: media_info.and_then(|m| serde_json::from_str(&m).ok()),
        custom_format_ids: from_json(r.try_get("custom_format_ids")?),
        custom_format_score: r.try_get::<i64, _>("custom_format_score")? as i32,
        date_added: from_ms(r.try_get("date_added")?),
    })
}

fn select(t: FileTable) -> String {
    let extra = match t {
        FileTable::Episode => ", season_number",
        FileTable::Movie => "",
    };
    format!("SELECT id, {}{extra}, {COLS} FROM {}", t.owner(), t.table())
}

/// Files of the given owners (movie ids / series ids), or all files.
pub async fn list(db: &Db, t: FileTable, owners: Option<&[i64]>) -> Result<Vec<FileRow>> {
    let sql = match owners {
        Some(o) => format!(
            "{} WHERE {} IN ({}) ORDER BY id",
            select(t),
            t.owner(),
            placeholders(o.len())
        ),
        None => format!("{} ORDER BY id", select(t)),
    };
    let mut q = sqlx::query(&sql);
    for id in owners.unwrap_or_default() {
        q = q.bind(id);
    }
    q.fetch_all(db.pool()).await?.iter().map(|r| map(t, r)).collect()
}

pub async fn get(db: &Db, t: FileTable, id: i64) -> Result<Option<FileRow>> {
    let row = sqlx::query(&format!("{} WHERE id = ?", select(t)))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    row.as_ref().map(|r| map(t, r)).transpose()
}

/// Inserts (or replaces the row with the same owner + relative path); returns the id.
pub async fn upsert(db: &Db, t: FileTable, f: &FileRow) -> Result<i64> {
    let now = now_ms();
    let (season_col, season_val) = match t {
        FileTable::Episode => (", season_number", ", ?"),
        FileTable::Movie => ("", ""),
    };
    let sql = format!(
        "INSERT INTO {table} ({owner}{season_col}, {COLS}, created_at, updated_at)
         VALUES (?{season_val}, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT({owner}, relative_path) DO UPDATE SET size = excluded.size, quality = excluded.quality,
             languages = excluded.languages, release_group = excluded.release_group, edition = excluded.edition,
             scene_name = COALESCE(excluded.scene_name, {table}.scene_name),
             original_path = COALESCE(excluded.original_path, {table}.original_path),
             media_info = COALESCE(excluded.media_info, {table}.media_info),
             custom_format_ids = excluded.custom_format_ids, custom_format_score = excluded.custom_format_score,
             updated_at = excluded.updated_at",
        table = t.table(),
        owner = t.owner(),
    );
    let mut q = sqlx::query(&sql).bind(f.owner_id);
    if t == FileTable::Episode {
        q = q.bind(f.season_number.unwrap_or(0) as i64);
    }
    q.bind(&f.relative_path)
        .bind(f.size as i64)
        .bind(json(&f.quality))
        .bind(json(&f.languages))
        .bind(&f.release_group)
        .bind(&f.edition)
        .bind(&f.scene_name)
        .bind(&f.original_path)
        .bind(f.media_info.as_ref().map(json))
        .bind(json(&f.custom_format_ids))
        .bind(f.custom_format_score as i64)
        .bind(f.date_added.timestamp_millis())
        .bind(now)
        .bind(now)
        .execute(db.pool())
        .await?;
    let id: i64 = sqlx::query_scalar(&format!(
        "SELECT id FROM {} WHERE {} = ? AND relative_path = ?",
        t.table(),
        t.owner()
    ))
    .bind(f.owner_id)
    .bind(&f.relative_path)
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

pub async fn set_relative_path(db: &Db, t: FileTable, id: i64, rel: &str, season: Option<u32>) -> Result<()> {
    let sql = match (t, season) {
        (FileTable::Episode, Some(_)) => format!(
            "UPDATE {} SET relative_path = ?, season_number = ?, updated_at = ? WHERE id = ?",
            t.table()
        ),
        _ => format!(
            "UPDATE {} SET relative_path = ?, updated_at = ? WHERE id = ?",
            t.table()
        ),
    };
    let mut q = sqlx::query(&sql).bind(rel);
    if let (FileTable::Episode, Some(s)) = (t, season) {
        q = q.bind(s as i64);
    }
    q.bind(now_ms()).bind(id).execute(db.pool()).await?;
    Ok(())
}

pub async fn set_scores(db: &Db, t: FileTable, id: i64, ids: &[i64], score: i32) -> Result<()> {
    sqlx::query(&format!(
        "UPDATE {} SET custom_format_ids = ?, custom_format_score = ?, updated_at = ? WHERE id = ?",
        t.table()
    ))
    .bind(json(&ids))
    .bind(score as i64)
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, t: FileTable, id: i64) -> Result<bool> {
    let res = sqlx::query(&format!("DELETE FROM {} WHERE id = ?", t.table()))
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}
