//! `albums` rows (MusicBrainz release groups) ⇄ [`AlbumDto`].

use chrono::NaiveDate;
use reelix_api_types::library::{ImagesDto, RatingsDto};
use reelix_api_types::music::{AlbumDto, AlbumListQuery, AlbumStatisticsDto, AlbumType, SecondaryAlbumType};
use reelix_db::{Db, from_ms, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{page_window, placeholders};
use crate::convert::{clean_name, date_str, enum_str, from_json, json, parse_enum};
use crate::error::{MusicError, Result};

const COLS: &str = "al.id, al.artist_id, ar.name AS artist_name, al.musicbrainz_release_group_id, al.title, \
    al.disambiguation, al.album_type, al.secondary_types, al.release_date, al.genres, al.images, al.ratings, \
    al.monitored, al.selected_release_id, al.any_release_ok, al.added_by_request_id, al.created_at, \
    (SELECT COUNT(*) FROM tracks t WHERE t.release_id = al.selected_release_id) AS s_tracks, \
    (SELECT COUNT(*) FROM tracks t WHERE t.release_id = al.selected_release_id AND t.track_file_id IS NOT NULL) \
        AS s_files, \
    (SELECT COALESCE(SUM(f.size), 0) FROM track_files f WHERE f.album_id = al.id) AS s_size";

const FROM: &str = "FROM albums al JOIN artists ar ON ar.id = al.artist_id";

fn map(r: &SqliteRow) -> Result<AlbumDto> {
    Ok(AlbumDto {
        id: r.try_get("id")?,
        artist_id: r.try_get("artist_id")?,
        artist_name: r.try_get("artist_name")?,
        musicbrainz_release_group_id: r.try_get("musicbrainz_release_group_id")?,
        title: r.try_get("title")?,
        disambiguation: r.try_get("disambiguation")?,
        album_type: parse_enum(&r.try_get::<String, _>("album_type")?).unwrap_or_default(),
        secondary_types: from_json(r.try_get("secondary_types")?),
        release_date: crate::convert::parse_date(r.try_get::<Option<String>, _>("release_date")?.as_deref()),
        genres: from_json(r.try_get("genres")?),
        images: from_json(r.try_get("images")?),
        ratings: from_json(r.try_get("ratings")?),
        monitored: r.try_get("monitored")?,
        selected_release_id: r.try_get("selected_release_id")?,
        any_release_ok: r.try_get("any_release_ok")?,
        releases: Vec::new(),
        statistics: AlbumStatisticsDto {
            track_count: r.try_get::<i64, _>("s_tracks")?.max(0) as u32,
            track_file_count: r.try_get::<i64, _>("s_files")?.max(0) as u32,
            size_on_disk: r.try_get::<i64, _>("s_size")?.max(0) as u64,
        },
        added_by_request_id: r.try_get("added_by_request_id")?,
        added: from_ms(r.try_get("created_at")?),
    })
}

async fn fetch(db: &Db, where_sql: &str, binds: &[i64]) -> Result<Vec<AlbumDto>> {
    let sql = format!(
        "SELECT {COLS} {FROM} {where_sql} ORDER BY al.release_date IS NULL, al.release_date, al.title COLLATE NOCASE"
    );
    let mut q = sqlx::query(&sql);
    for b in binds {
        q = q.bind(b);
    }
    q.fetch_all(db.pool()).await?.iter().map(map).collect()
}

/// Albums (all, or `ids`).
pub async fn load(db: &Db, ids: Option<&[i64]>) -> Result<Vec<AlbumDto>> {
    match ids {
        Some(ids) => fetch(db, &format!("WHERE al.id IN ({})", placeholders(ids.len())), ids).await,
        None => fetch(db, "", &[]).await,
    }
}

pub async fn of_artists(db: &Db, artist_ids: &[i64]) -> Result<Vec<AlbumDto>> {
    fetch(
        db,
        &format!("WHERE al.artist_id IN ({})", placeholders(artist_ids.len())),
        artist_ids,
    )
    .await
}

pub async fn get(db: &Db, id: i64) -> Result<Option<AlbumDto>> {
    Ok(load(db, Some(&[id])).await?.pop())
}

pub async fn require(db: &Db, id: i64) -> Result<AlbumDto> {
    get(db, id)
        .await?
        .ok_or_else(|| MusicError::NotFound(format!("album {id} not found")))
}

pub async fn by_mbid(db: &Db, rg: &str) -> Result<Option<AlbumDto>> {
    let sql = format!("SELECT {COLS} {FROM} WHERE al.musicbrainz_release_group_id = ?");
    let row = sqlx::query(&sql).bind(rg).fetch_optional(db.pool()).await?;
    row.as_ref().map(map).transpose()
}

/// `(album id, clean title)` of an artist's albums.
pub async fn clean_titles(db: &Db, artist_id: i64) -> Result<Vec<(i64, String)>> {
    let rows = sqlx::query("SELECT id, clean_title FROM albums WHERE artist_id = ?")
        .bind(artist_id)
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| Ok((r.try_get("id")?, r.try_get("clean_title")?)))
        .collect()
}

/// Whether the album's releases were fetched at least once.
pub async fn releases_synced(db: &Db, id: i64) -> Result<bool> {
    let v: Option<Option<i64>> = sqlx::query_scalar("SELECT last_refreshed FROM albums WHERE id = ?")
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    Ok(v.flatten().is_some())
}

pub async fn page(db: &Db, q: &AlbumListQuery) -> Result<(Vec<AlbumDto>, u64, u32, u32)> {
    let mut wh: Vec<&str> = Vec::new();
    if q.artist_id.is_some() {
        wh.push("al.artist_id = ?");
    }
    let like = q
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));
    if like.is_some() {
        wh.push("(al.title LIKE ? OR ar.name LIKE ?)");
    }
    if q.monitored.is_some() {
        wh.push("al.monitored = ?");
    }
    match q.missing {
        Some(true) => wh.push(
            "al.monitored = 1 AND ((SELECT COUNT(*) FROM tracks t WHERE t.release_id = al.selected_release_id \
             AND t.track_file_id IS NULL) > 0 OR (al.selected_release_id IS NULL AND \
             (SELECT COUNT(*) FROM track_files f WHERE f.album_id = al.id) = 0))",
        ),
        Some(false) => wh.push(
            "(SELECT COUNT(*) FROM tracks t WHERE t.release_id = al.selected_release_id \
             AND t.track_file_id IS NULL) = 0 AND al.selected_release_id IS NOT NULL",
        ),
        None => {}
    }
    let where_sql = if wh.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", wh.join(" AND "))
    };
    let (page, size, window) = page_window(q.page, q.page_size);
    let limit = window.map_or(String::new(), |(o, l)| format!(" LIMIT {l} OFFSET {o}"));
    let list_sql = format!(
        "SELECT {COLS} {FROM} {where_sql} ORDER BY al.release_date IS NULL, al.release_date DESC, al.id{limit}"
    );
    let count_sql = format!("SELECT COUNT(*) AS n {FROM} {where_sql}");
    let mut lq = sqlx::query(&list_sql);
    let mut cq = sqlx::query(&count_sql);
    if let Some(a) = q.artist_id {
        lq = lq.bind(a);
        cq = cq.bind(a);
    }
    if let Some(l) = &like {
        lq = lq.bind(l.clone()).bind(l.clone());
        cq = cq.bind(l.clone()).bind(l.clone());
    }
    if let Some(m) = q.monitored {
        lq = lq.bind(m);
        cq = cq.bind(m);
    }
    let items = lq
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map)
        .collect::<Result<Vec<_>>>()?;
    let total: i64 = cq.fetch_one(db.pool()).await?.try_get("n")?;
    Ok((items, total.max(0) as u64, page, size))
}

/// Monitored albums of monitored artists released on or before `today` (or undated).
pub async fn wanted_candidates(db: &Db, today: NaiveDate) -> Result<Vec<AlbumDto>> {
    let sql = format!(
        "SELECT {COLS} {FROM} WHERE al.monitored = 1 AND ar.monitored = 1 \
         AND (al.release_date IS NULL OR al.release_date <= ?) \
         ORDER BY al.release_date DESC, al.id"
    );
    let rows = sqlx::query(&sql)
        .bind(today.format("%Y-%m-%d").to_string())
        .fetch_all(db.pool())
        .await?;
    rows.iter().map(map).collect()
}

/// Descriptive columns written on insert and refresh.
#[derive(Debug, Clone)]
pub struct AlbumFields {
    pub musicbrainz_release_group_id: String,
    pub title: String,
    pub disambiguation: Option<String>,
    pub album_type: AlbumType,
    pub secondary_types: Vec<SecondaryAlbumType>,
    pub release_date: Option<NaiveDate>,
    pub genres: Vec<String>,
    pub images: ImagesDto,
    pub ratings: RatingsDto,
}

pub async fn insert(
    db: &Db,
    artist_id: i64,
    f: &AlbumFields,
    monitored: bool,
    added_by_request_id: Option<i64>,
) -> Result<i64> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO albums (artist_id, musicbrainz_release_group_id, title, clean_title, disambiguation, album_type,
            secondary_types, release_date, genres, images, ratings, monitored, added_by_request_id, created_at,
            updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(artist_id)
    .bind(&f.musicbrainz_release_group_id)
    .bind(&f.title)
    .bind(clean_name(&f.title))
    .bind(&f.disambiguation)
    .bind(enum_str(&f.album_type))
    .bind(json(&f.secondary_types))
    .bind(date_str(f.release_date))
    .bind(json(&f.genres))
    .bind(json(&f.images))
    .bind(json(&f.ratings))
    .bind(monitored)
    .bind(added_by_request_id)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => MusicError::Conflict(format!(
            "album {} is already in the library",
            f.musicbrainz_release_group_id
        )),
        _ => e.into(),
    })?;
    Ok(res.last_insert_rowid())
}

/// Refresh: replaces descriptive columns (images only when the new set has any).
pub async fn update_fields(db: &Db, id: i64, f: &AlbumFields) -> Result<()> {
    let images = (f.images != ImagesDto::default()).then(|| json(&f.images));
    sqlx::query(
        "UPDATE albums SET title = ?, clean_title = ?, disambiguation = ?, album_type = ?, secondary_types = ?,
            release_date = ?, genres = ?, images = COALESCE(?, images), ratings = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&f.title)
    .bind(clean_name(&f.title))
    .bind(&f.disambiguation)
    .bind(enum_str(&f.album_type))
    .bind(json(&f.secondary_types))
    .bind(date_str(f.release_date))
    .bind(json(&f.genres))
    .bind(images)
    .bind(json(&f.ratings))
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn set_images(db: &Db, id: i64, images: &ImagesDto) -> Result<()> {
    sqlx::query("UPDATE albums SET images = ?, updated_at = ? WHERE id = ?")
        .bind(json(images))
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn set_monitored(db: &Db, ids: &[i64], monitored: bool) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "UPDATE albums SET monitored = ?, updated_at = ? WHERE id IN ({})",
        placeholders(ids.len())
    );
    let mut q = sqlx::query(&sql).bind(monitored).bind(now_ms());
    for id in ids {
        q = q.bind(id);
    }
    q.execute(db.pool()).await?;
    Ok(())
}

pub async fn set_any_release_ok(db: &Db, id: i64, v: bool) -> Result<()> {
    sqlx::query("UPDATE albums SET any_release_ok = ?, updated_at = ? WHERE id = ?")
        .bind(v)
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn set_selected_release(db: &Db, id: i64, release_id: Option<i64>) -> Result<()> {
    sqlx::query("UPDATE albums SET selected_release_id = ?, updated_at = ? WHERE id = ?")
        .bind(release_id)
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Stamps the release sync time.
pub async fn mark_synced(db: &Db, id: i64) -> Result<()> {
    let now = now_ms();
    sqlx::query("UPDATE albums SET last_refreshed = ?, updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM albums WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}
