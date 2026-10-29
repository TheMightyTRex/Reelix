//! `book_files` rows (one per book and format).

use std::collections::HashMap;
use std::path::Path;

use reelix_api_types::books::{BookFileDto, BookFilePartDto};
use reelix_core::BookFormat;
use reelix_db::{Db, from_ms, now_ms};
use reelix_decision::QualityProfile;
use reelix_parser::QualityModel;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{from_json, json, placeholders};
use crate::error::Result;

const COLS: &str = "id, book_id, format, relative_path, parts, size, quality, bitrate_kbps, duration_seconds, \
    narrator, edition_id, release_group, scene_name, original_path, custom_format_ids, custom_format_score, date_added";

#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    pub id: i64,
    pub book_id: i64,
    pub format: BookFormat,
    /// Relative to the author's path (ebook) or audio path (audiobook); a folder for
    /// multi-file audiobooks.
    pub relative_path: String,
    /// Multi-file audiobooks: every part, relative to the same base.
    pub parts: Vec<BookFilePartDto>,
    pub size: u64,
    pub quality: QualityModel,
    pub bitrate_kbps: Option<u32>,
    pub duration_seconds: Option<u32>,
    pub narrator: Option<String>,
    pub edition_id: Option<i64>,
    pub release_group: Option<String>,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    pub custom_format_ids: Vec<i64>,
    pub custom_format_score: i32,
    pub date_added: i64,
}

impl FileRow {
    pub fn new(book_id: i64, format: BookFormat, relative_path: String, quality: QualityModel) -> Self {
        Self {
            id: 0,
            book_id,
            format,
            relative_path,
            parts: Vec::new(),
            size: 0,
            quality,
            bitrate_kbps: None,
            duration_seconds: None,
            narrator: None,
            edition_id: None,
            release_group: None,
            scene_name: None,
            original_path: None,
            custom_format_ids: Vec::new(),
            custom_format_score: 0,
            date_added: now_ms(),
        }
    }

    /// Relative paths of every file of this record (the parts, or the single file).
    pub fn file_paths(&self) -> Vec<String> {
        if self.parts.is_empty() {
            vec![self.relative_path.clone()]
        } else {
            self.parts.iter().map(|p| p.relative_path.clone()).collect()
        }
    }

    pub fn to_dto(
        &self,
        base: Option<&str>,
        profile: Option<&QualityProfile>,
        names: &HashMap<i64, String>,
    ) -> BookFileDto {
        let path = match base {
            Some(b) => Path::new(b)
                .join(&self.relative_path)
                .to_string_lossy()
                .into_owned(),
            None => self.relative_path.clone(),
        };
        BookFileDto {
            id: self.id,
            book_id: self.book_id,
            format: self.format,
            relative_path: self.relative_path.clone(),
            path,
            parts: self.parts.clone(),
            size: self.size,
            quality: self.quality,
            quality_name: self.quality.quality.name().to_string(),
            bitrate_kbps: self.bitrate_kbps,
            duration_seconds: self.duration_seconds,
            narrator: self.narrator.clone(),
            edition_id: self.edition_id,
            release_group: self.release_group.clone(),
            scene_name: self.scene_name.clone(),
            original_path: self.original_path.clone(),
            custom_format_ids: self.custom_format_ids.clone(),
            custom_formats: self
                .custom_format_ids
                .iter()
                .filter_map(|id| names.get(id).cloned())
                .collect(),
            custom_format_score: self.custom_format_score,
            cutoff_met: profile.is_some_and(|p| p.cutoff_met(self.quality.quality, self.custom_format_score)),
            date_added: from_ms(self.date_added),
        }
    }
}

fn map(r: &SqliteRow) -> Result<FileRow> {
    let format: String = r.try_get("format")?;
    Ok(FileRow {
        id: r.try_get("id")?,
        book_id: r.try_get("book_id")?,
        format: BookFormat::parse(&format).unwrap_or(BookFormat::Ebook),
        relative_path: r.try_get("relative_path")?,
        parts: from_json(r.try_get("parts")?),
        size: r.try_get::<i64, _>("size")?.max(0) as u64,
        quality: serde_json::from_str(&r.try_get::<String, _>("quality")?)
            .unwrap_or(QualityModel::new(reelix_parser::Quality::UnknownText)),
        bitrate_kbps: r.try_get::<Option<i64>, _>("bitrate_kbps")?.map(|v| v as u32),
        duration_seconds: r.try_get::<Option<i64>, _>("duration_seconds")?.map(|v| v as u32),
        narrator: r.try_get("narrator")?,
        edition_id: r.try_get("edition_id")?,
        release_group: r.try_get("release_group")?,
        scene_name: r.try_get("scene_name")?,
        original_path: r.try_get("original_path")?,
        custom_format_ids: from_json(r.try_get("custom_format_ids")?),
        custom_format_score: r.try_get::<i64, _>("custom_format_score")? as i32,
        date_added: r.try_get("date_added")?,
    })
}

/// Files of the given books (all when `None`).
pub async fn list(db: &Db, book_ids: Option<&[i64]>) -> Result<Vec<FileRow>> {
    let sql = match book_ids {
        Some(ids) => format!(
            "SELECT {COLS} FROM book_files WHERE book_id IN ({}) ORDER BY id",
            placeholders(ids.len())
        ),
        None => format!("SELECT {COLS} FROM book_files ORDER BY id"),
    };
    let mut q = sqlx::query(&sql);
    for id in book_ids.unwrap_or_default() {
        q = q.bind(id);
    }
    q.fetch_all(db.pool()).await?.iter().map(map).collect()
}

pub async fn get(db: &Db, id: i64) -> Result<Option<FileRow>> {
    let r = sqlx::query(&format!("SELECT {COLS} FROM book_files WHERE id = ?"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    r.as_ref().map(map).transpose()
}

pub async fn for_book(db: &Db, book_id: i64, format: BookFormat) -> Result<Option<FileRow>> {
    let r = sqlx::query(&format!(
        "SELECT {COLS} FROM book_files WHERE book_id = ? AND format = ?"
    ))
    .bind(book_id)
    .bind(format.as_str())
    .fetch_optional(db.pool())
    .await?;
    r.as_ref().map(map).transpose()
}

/// Inserts the file of `(book_id, format)` or replaces the existing one; returns its id.
pub async fn upsert(db: &Db, f: &FileRow) -> Result<i64> {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO book_files (book_id, format, relative_path, parts, size, quality, bitrate_kbps, duration_seconds, \
         narrator, edition_id, release_group, scene_name, original_path, custom_format_ids, custom_format_score, \
         date_added, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(book_id, format) DO UPDATE SET relative_path = excluded.relative_path, parts = excluded.parts, \
         size = excluded.size, quality = excluded.quality, bitrate_kbps = excluded.bitrate_kbps, \
         duration_seconds = excluded.duration_seconds, narrator = excluded.narrator, edition_id = excluded.edition_id, \
         release_group = excluded.release_group, scene_name = excluded.scene_name, original_path = excluded.original_path, \
         custom_format_ids = excluded.custom_format_ids, custom_format_score = excluded.custom_format_score, \
         date_added = excluded.date_added, updated_at = excluded.updated_at",
    )
    .bind(f.book_id)
    .bind(f.format.as_str())
    .bind(&f.relative_path)
    .bind(json(&f.parts))
    .bind(f.size as i64)
    .bind(json(&f.quality))
    .bind(f.bitrate_kbps)
    .bind(f.duration_seconds)
    .bind(&f.narrator)
    .bind(f.edition_id)
    .bind(&f.release_group)
    .bind(&f.scene_name)
    .bind(&f.original_path)
    .bind(json(&f.custom_format_ids))
    .bind(f.custom_format_score)
    .bind(f.date_added)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM book_files WHERE book_id = ? AND format = ?")
        .bind(f.book_id)
        .bind(f.format.as_str())
        .fetch_one(db.pool())
        .await?;
    Ok(id)
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM book_files WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}
