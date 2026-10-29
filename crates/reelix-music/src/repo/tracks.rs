//! `tracks` rows ⇄ [`TrackDto`].

use reelix_api_types::music::TrackDto;
use reelix_db::{Db, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::placeholders;
use crate::error::Result;

const COLS: &str = "id, album_id, release_id, musicbrainz_track_id, musicbrainz_recording_id, disc_number, \
    track_number, absolute_number, title, duration_ms, artist_credit, track_file_id";

fn map(r: &SqliteRow) -> Result<TrackDto> {
    Ok(TrackDto {
        id: r.try_get("id")?,
        album_id: r.try_get("album_id")?,
        release_id: r.try_get("release_id")?,
        musicbrainz_track_id: r.try_get("musicbrainz_track_id")?,
        musicbrainz_recording_id: r.try_get("musicbrainz_recording_id")?,
        disc_number: r.try_get::<i64, _>("disc_number")?.max(0) as u32,
        track_number: r.try_get::<i64, _>("track_number")?.max(0) as u32,
        absolute_number: r
            .try_get::<Option<i64>, _>("absolute_number")?
            .map(|v| v.max(0) as u32),
        title: r.try_get("title")?,
        duration_ms: r
            .try_get::<Option<i64>, _>("duration_ms")?
            .map(|v| v.max(0) as u64),
        artist_credit: r.try_get("artist_credit")?,
        track_file_id: r.try_get("track_file_id")?,
        track_file: None,
    })
}

/// Tracks of one release, in disc/track order.
pub async fn of_release(db: &Db, release_id: i64) -> Result<Vec<TrackDto>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLS} FROM tracks WHERE release_id = ? ORDER BY disc_number, track_number"
    ))
    .bind(release_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(map).collect()
}

/// Every track of an album (all releases).
pub async fn of_album(db: &Db, album_id: i64) -> Result<Vec<TrackDto>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLS} FROM tracks WHERE album_id = ? ORDER BY release_id, disc_number, track_number"
    ))
    .bind(album_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(map).collect()
}

/// A track to store (id ignored).
#[derive(Debug, Clone)]
pub struct NewTrack {
    pub musicbrainz_track_id: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub disc_number: u32,
    pub track_number: u32,
    pub absolute_number: u32,
    pub number: Option<String>,
    pub title: String,
    pub duration_ms: Option<u64>,
    pub artist_credit: Option<String>,
}

/// Replaces the tracklist of a release, keeping file links of tracks that stay.
pub async fn replace(db: &Db, album_id: i64, release_id: i64, tracks: &[NewTrack]) -> Result<()> {
    let now = now_ms();
    let mut tx = db.pool().begin().await?;
    let mut keep = Vec::new();
    for t in tracks {
        sqlx::query(
            "INSERT INTO tracks (album_id, release_id, musicbrainz_track_id, musicbrainz_recording_id, disc_number,
                track_number, absolute_number, number, title, duration_ms, artist_credit, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(release_id, disc_number, track_number) DO UPDATE SET
                musicbrainz_track_id = excluded.musicbrainz_track_id,
                musicbrainz_recording_id = excluded.musicbrainz_recording_id,
                absolute_number = excluded.absolute_number, number = excluded.number, title = excluded.title,
                duration_ms = excluded.duration_ms, artist_credit = excluded.artist_credit,
                updated_at = excluded.updated_at",
        )
        .bind(album_id)
        .bind(release_id)
        .bind(&t.musicbrainz_track_id)
        .bind(&t.musicbrainz_recording_id)
        .bind(i64::from(t.disc_number))
        .bind(i64::from(t.track_number))
        .bind(i64::from(t.absolute_number))
        .bind(&t.number)
        .bind(&t.title)
        .bind(t.duration_ms.map(|d| d as i64))
        .bind(&t.artist_credit)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        let id: i64 = sqlx::query_scalar(
            "SELECT id FROM tracks WHERE release_id = ? AND disc_number = ? AND track_number = ?",
        )
        .bind(release_id)
        .bind(i64::from(t.disc_number))
        .bind(i64::from(t.track_number))
        .fetch_one(&mut *tx)
        .await?;
        keep.push(id);
    }
    let sql = format!(
        "DELETE FROM tracks WHERE release_id = ? AND id NOT IN ({})",
        placeholders(keep.len())
    );
    let mut q = sqlx::query(&sql).bind(release_id);
    for id in &keep {
        q = q.bind(id);
    }
    q.execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Points `track_ids` at `file_id` (or clears them with `None`).
pub async fn link_file(db: &Db, track_ids: &[i64], file_id: Option<i64>) -> Result<()> {
    if track_ids.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "UPDATE tracks SET track_file_id = ?, updated_at = ? WHERE id IN ({})",
        placeholders(track_ids.len())
    );
    let mut q = sqlx::query(&sql).bind(file_id).bind(now_ms());
    for id in track_ids {
        q = q.bind(id);
    }
    q.execute(db.pool()).await?;
    Ok(())
}

/// Clears every file link of an album.
pub async fn unlink_album(db: &Db, album_id: i64) -> Result<()> {
    sqlx::query("UPDATE tracks SET track_file_id = NULL, updated_at = ? WHERE album_id = ?")
        .bind(now_ms())
        .bind(album_id)
        .execute(db.pool())
        .await?;
    Ok(())
}
