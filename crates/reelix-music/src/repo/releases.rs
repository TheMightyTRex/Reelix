//! `album_releases` rows (MusicBrainz releases / editions) ⇄ [`AlbumReleaseDto`].

use chrono::NaiveDate;
use reelix_api_types::music::{AlbumReleaseDto, ReleaseStatus};
use reelix_db::{Db, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::convert::{date_str, enum_str, parse_date, parse_enum};
use crate::error::Result;

const COLS: &str = "id, album_id, musicbrainz_release_id, title, status, country, label, catalog_number, barcode, \
    format, disc_count, track_count, release_date, disambiguation";

/// A stored release.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseRow {
    pub id: i64,
    pub album_id: i64,
    pub musicbrainz_release_id: String,
    pub title: String,
    pub status: Option<ReleaseStatus>,
    pub country: Option<String>,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
    pub barcode: Option<String>,
    pub format: Option<String>,
    pub disc_count: u32,
    pub track_count: u32,
    pub release_date: Option<NaiveDate>,
    pub disambiguation: Option<String>,
}

impl ReleaseRow {
    pub fn to_dto(&self, selected: Option<i64>) -> AlbumReleaseDto {
        AlbumReleaseDto {
            id: self.id,
            musicbrainz_release_id: self.musicbrainz_release_id.clone(),
            title: self.title.clone(),
            status: self.status,
            country: self.country.clone(),
            label: self.label.clone(),
            catalog_number: self.catalog_number.clone(),
            format: self.format.clone(),
            disc_count: self.disc_count,
            track_count: self.track_count,
            release_date: self.release_date,
            disambiguation: self.disambiguation.clone(),
            selected: selected == Some(self.id),
        }
    }
}

fn map(r: &SqliteRow) -> Result<ReleaseRow> {
    Ok(ReleaseRow {
        id: r.try_get("id")?,
        album_id: r.try_get("album_id")?,
        musicbrainz_release_id: r.try_get("musicbrainz_release_id")?,
        title: r.try_get("title")?,
        status: r
            .try_get::<Option<String>, _>("status")?
            .and_then(|s| parse_enum(&s)),
        country: r.try_get("country")?,
        label: r.try_get("label")?,
        catalog_number: r.try_get("catalog_number")?,
        barcode: r.try_get("barcode")?,
        format: r.try_get("format")?,
        disc_count: r.try_get::<i64, _>("disc_count")?.max(0) as u32,
        track_count: r.try_get::<i64, _>("track_count")?.max(0) as u32,
        release_date: parse_date(r.try_get::<Option<String>, _>("release_date")?.as_deref()),
        disambiguation: r.try_get("disambiguation")?,
    })
}

/// Releases of an album, ordered by date.
pub async fn of_album(db: &Db, album_id: i64) -> Result<Vec<ReleaseRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLS} FROM album_releases WHERE album_id = ? ORDER BY release_date IS NULL, release_date, id"
    ))
    .bind(album_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(map).collect()
}

pub async fn get(db: &Db, id: i64) -> Result<Option<ReleaseRow>> {
    let row = sqlx::query(&format!("SELECT {COLS} FROM album_releases WHERE id = ?"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    row.as_ref().map(map).transpose()
}

/// Album id of a release by MusicBrainz release id.
pub async fn album_of_mbid(db: &Db, mbid: &str) -> Result<Option<(i64, i64)>> {
    let row = sqlx::query("SELECT id, album_id FROM album_releases WHERE musicbrainz_release_id = ?")
        .bind(mbid)
        .fetch_optional(db.pool())
        .await?;
    row.map(|r| Ok((r.try_get("id")?, r.try_get("album_id")?)))
        .transpose()
}

/// Inserts or updates by MusicBrainz release id; returns the row id.
pub async fn upsert(db: &Db, r: &ReleaseRow) -> Result<i64> {
    let now = now_ms();
    sqlx::query(
        "INSERT INTO album_releases (album_id, musicbrainz_release_id, title, status, country, label, catalog_number,
            barcode, format, disc_count, track_count, release_date, disambiguation, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(musicbrainz_release_id) DO UPDATE SET album_id = excluded.album_id, title = excluded.title,
            status = excluded.status, country = excluded.country, label = excluded.label,
            catalog_number = excluded.catalog_number, barcode = excluded.barcode, format = excluded.format,
            disc_count = excluded.disc_count, track_count = excluded.track_count,
            release_date = excluded.release_date, disambiguation = excluded.disambiguation,
            updated_at = excluded.updated_at",
    )
    .bind(r.album_id)
    .bind(&r.musicbrainz_release_id)
    .bind(&r.title)
    .bind(r.status.map(|s| enum_str(&s)))
    .bind(&r.country)
    .bind(&r.label)
    .bind(&r.catalog_number)
    .bind(&r.barcode)
    .bind(&r.format)
    .bind(i64::from(r.disc_count))
    .bind(i64::from(r.track_count))
    .bind(date_str(r.release_date))
    .bind(&r.disambiguation)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM album_releases WHERE musicbrainz_release_id = ?")
        .bind(&r.musicbrainz_release_id)
        .fetch_one(db.pool())
        .await?;
    Ok(id)
}

/// Deletes the album's releases that are not in `keep` (their tracks cascade).
pub async fn delete_others(db: &Db, album_id: i64, keep: &[i64]) -> Result<()> {
    let sql = format!(
        "DELETE FROM album_releases WHERE album_id = ? AND id NOT IN ({})",
        super::placeholders(keep.len())
    );
    let mut q = sqlx::query(&sql).bind(album_id);
    for id in keep {
        q = q.bind(id);
    }
    q.execute(db.pool()).await?;
    Ok(())
}
