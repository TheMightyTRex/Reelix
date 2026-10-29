//! `track_files` rows ⇄ [`TrackFileDto`].

use std::path::Path;

use chrono::{DateTime, Utc};
use reelix_api_types::music::TrackFileDto;
use reelix_db::{Db, from_ms, now_ms};
use reelix_decision::QualityProfile;
use reelix_parser::{Quality, QualityModel};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::placeholders;
use crate::convert::{from_json, json};
use crate::error::Result;

/// A stored audio file of an album.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    pub id: i64,
    pub album_id: i64,
    /// Relative to the artist folder.
    pub relative_path: String,
    pub size: u64,
    pub quality: QualityModel,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub duration_ms: Option<u64>,
    /// Tags read from the file.
    pub tags: serde_json::Value,
    pub release_group: Option<String>,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    pub custom_format_ids: Vec<i64>,
    pub custom_format_score: i32,
    pub date_added: DateTime<Utc>,
}

impl FileRow {
    pub fn to_dto(
        &self,
        artist_path: &str,
        profile: Option<&QualityProfile>,
        format_names: &std::collections::HashMap<i64, String>,
    ) -> TrackFileDto {
        TrackFileDto {
            id: self.id,
            album_id: self.album_id,
            relative_path: self.relative_path.clone(),
            path: Path::new(artist_path)
                .join(&self.relative_path)
                .to_string_lossy()
                .into_owned(),
            size: self.size,
            quality: self.quality,
            quality_name: self.quality.quality.name().to_owned(),
            bitrate_kbps: self.bitrate_kbps,
            sample_rate_hz: self.sample_rate_hz,
            bit_depth: self.bit_depth,
            channels: self.channels,
            duration_ms: self.duration_ms,
            release_group: self.release_group.clone(),
            scene_name: self.scene_name.clone(),
            original_path: self.original_path.clone(),
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

const COLS: &str = "id, album_id, relative_path, size, quality, bitrate_kbps, sample_rate_hz, bit_depth, channels, \
    duration_ms, tags, release_group, scene_name, original_path, custom_format_ids, custom_format_score, date_added";

fn map(r: &SqliteRow) -> Result<FileRow> {
    let quality: Option<String> = r.try_get("quality")?;
    let opt_u32 = |c: &str| -> Result<Option<u32>> {
        Ok(r.try_get::<Option<i64>, _>(c)?
            .map(|v| v.clamp(0, i64::from(u32::MAX)) as u32))
    };
    Ok(FileRow {
        id: r.try_get("id")?,
        album_id: r.try_get("album_id")?,
        relative_path: r.try_get("relative_path")?,
        size: r.try_get::<i64, _>("size")?.max(0) as u64,
        quality: quality
            .and_then(|q| serde_json::from_str(&q).ok())
            .unwrap_or(QualityModel::new(Quality::UnknownMusic)),
        bitrate_kbps: opt_u32("bitrate_kbps")?,
        sample_rate_hz: opt_u32("sample_rate_hz")?,
        bit_depth: opt_u32("bit_depth")?.map(|v| v.min(255) as u8),
        channels: opt_u32("channels")?.map(|v| v.min(255) as u8),
        duration_ms: r
            .try_get::<Option<i64>, _>("duration_ms")?
            .map(|v| v.max(0) as u64),
        tags: r
            .try_get::<Option<String>, _>("tags")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null),
        release_group: r.try_get("release_group")?,
        scene_name: r.try_get("scene_name")?,
        original_path: r.try_get("original_path")?,
        custom_format_ids: from_json(r.try_get("custom_format_ids")?),
        custom_format_score: r.try_get::<i64, _>("custom_format_score")? as i32,
        date_added: from_ms(r.try_get("date_added")?),
    })
}

/// Files of the given albums.
pub async fn of_albums(db: &Db, album_ids: &[i64]) -> Result<Vec<FileRow>> {
    let sql = format!(
        "SELECT {COLS} FROM track_files WHERE album_id IN ({}) ORDER BY relative_path",
        placeholders(album_ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in album_ids {
        q = q.bind(id);
    }
    q.fetch_all(db.pool()).await?.iter().map(map).collect()
}

/// Files of an artist (every album).
pub async fn of_artist(db: &Db, artist_id: i64) -> Result<Vec<FileRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLS} FROM track_files WHERE album_id IN (SELECT id FROM albums WHERE artist_id = ?) \
         ORDER BY relative_path"
    ))
    .bind(artist_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(map).collect()
}

pub async fn get(db: &Db, id: i64) -> Result<Option<FileRow>> {
    let row = sqlx::query(&format!("SELECT {COLS} FROM track_files WHERE id = ?"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    row.as_ref().map(map).transpose()
}

/// Inserts (or replaces the row with the same album + relative path); returns the id.
pub async fn upsert(db: &Db, f: &FileRow) -> Result<i64> {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO track_files (album_id, relative_path, size, quality, bitrate_kbps, sample_rate_hz, bit_depth,
            channels, duration_ms, tags, release_group, scene_name, original_path, custom_format_ids,
            custom_format_score, date_added, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(album_id, relative_path) DO UPDATE SET size = excluded.size, quality = excluded.quality,
            bitrate_kbps = excluded.bitrate_kbps, sample_rate_hz = excluded.sample_rate_hz,
            bit_depth = excluded.bit_depth, channels = excluded.channels, duration_ms = excluded.duration_ms,
            tags = excluded.tags, release_group = COALESCE(excluded.release_group, track_files.release_group),
            scene_name = COALESCE(excluded.scene_name, track_files.scene_name),
            original_path = COALESCE(excluded.original_path, track_files.original_path),
            custom_format_ids = excluded.custom_format_ids, custom_format_score = excluded.custom_format_score,
            updated_at = excluded.updated_at",
    )
    .bind(f.album_id)
    .bind(&f.relative_path)
    .bind(f.size as i64)
    .bind(json(&f.quality))
    .bind(f.bitrate_kbps.map(i64::from))
    .bind(f.sample_rate_hz.map(i64::from))
    .bind(f.bit_depth.map(i64::from))
    .bind(f.channels.map(i64::from))
    .bind(f.duration_ms.map(|d| d as i64))
    .bind(json(&f.tags))
    .bind(&f.release_group)
    .bind(&f.scene_name)
    .bind(&f.original_path)
    .bind(json(&f.custom_format_ids))
    .bind(i64::from(f.custom_format_score))
    .bind(f.date_added.timestamp_millis())
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM track_files WHERE album_id = ? AND relative_path = ?")
        .bind(f.album_id)
        .bind(&f.relative_path)
        .fetch_one(db.pool())
        .await?;
    Ok(id)
}

pub async fn set_relative_path(db: &Db, id: i64, rel: &str) -> Result<()> {
    sqlx::query("UPDATE track_files SET relative_path = ?, updated_at = ? WHERE id = ?")
        .bind(rel)
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Deletes a file row (track links are cleared by `ON DELETE SET NULL`).
pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM track_files WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}
