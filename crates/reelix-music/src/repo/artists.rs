//! `artists` rows ⇄ [`ArtistDto`].

use reelix_api_types::library::{ImagesDto, SortDir};
use reelix_api_types::music::{ArtistDto, ArtistListQuery, ArtistStatisticsDto, NewAlbumMonitor};
use reelix_db::{Db, from_ms, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{page_window, placeholders};
use crate::convert::{enum_str, from_json, json, parse_enum};
use crate::error::{MusicError, Result};

const COLS: &str = "a.id, a.musicbrainz_id, a.name, a.sort_name, a.disambiguation, a.artist_type, a.country, \
    a.overview, a.genres, a.images, a.links, a.monitored, a.monitor_new_items, a.quality_profile_id, \
    a.metadata_profile_id, a.root_folder_id, a.path, a.tags, a.last_refreshed, a.created_at, a.custom, \
    (SELECT COUNT(*) FROM albums al WHERE al.artist_id = a.id) AS s_albums, \
    (SELECT COUNT(*) FROM tracks t JOIN albums al ON al.id = t.album_id \
        WHERE al.artist_id = a.id AND al.monitored = 1 AND t.release_id = al.selected_release_id) AS s_tracks, \
    (SELECT COUNT(*) FROM track_files f JOIN albums al ON al.id = f.album_id WHERE al.artist_id = a.id) AS s_files, \
    (SELECT COALESCE(SUM(f.size), 0) FROM track_files f JOIN albums al ON al.id = f.album_id \
        WHERE al.artist_id = a.id) AS s_size";

fn map(r: &SqliteRow) -> Result<ArtistDto> {
    Ok(ArtistDto {
        id: r.try_get("id")?,
        musicbrainz_id: r.try_get("musicbrainz_id")?,
        name: r.try_get("name")?,
        sort_name: r.try_get("sort_name")?,
        disambiguation: r.try_get("disambiguation")?,
        artist_type: r.try_get("artist_type")?,
        country: r.try_get("country")?,
        overview: r.try_get("overview")?,
        genres: from_json(r.try_get("genres")?),
        images: from_json(r.try_get("images")?),
        links: from_json(r.try_get("links")?),
        monitored: r.try_get("monitored")?,
        monitor_new_items: parse_enum(&r.try_get::<String, _>("monitor_new_items")?).unwrap_or_default(),
        quality_profile_id: r.try_get("quality_profile_id")?,
        metadata_profile_id: r.try_get("metadata_profile_id")?,
        root_folder_id: r.try_get("root_folder_id")?,
        path: r.try_get("path")?,
        tags: from_json(r.try_get("tags")?),
        statistics: ArtistStatisticsDto {
            album_count: r.try_get::<i64, _>("s_albums")?.max(0) as u32,
            track_count: r.try_get::<i64, _>("s_tracks")?.max(0) as u32,
            track_file_count: r.try_get::<i64, _>("s_files")?.max(0) as u32,
            size_on_disk: r.try_get::<i64, _>("s_size")?.max(0) as u64,
        },
        added: from_ms(r.try_get("created_at")?),
        last_refreshed: r.try_get::<Option<i64>, _>("last_refreshed")?.map(from_ms),
        custom: r.try_get("custom")?,
    })
}

/// Artists (all, or `ids`), ordered by sort name.
pub async fn load(db: &Db, ids: Option<&[i64]>) -> Result<Vec<ArtistDto>> {
    let sql = match ids {
        Some(ids) => format!(
            "SELECT {COLS} FROM artists a WHERE a.id IN ({}) ORDER BY a.sort_name COLLATE NOCASE",
            placeholders(ids.len())
        ),
        None => format!("SELECT {COLS} FROM artists a ORDER BY a.sort_name COLLATE NOCASE"),
    };
    let mut q = sqlx::query(&sql);
    for id in ids.unwrap_or_default() {
        q = q.bind(id);
    }
    q.fetch_all(db.pool()).await?.iter().map(map).collect()
}

pub async fn get(db: &Db, id: i64) -> Result<Option<ArtistDto>> {
    Ok(load(db, Some(&[id])).await?.pop())
}

pub async fn require(db: &Db, id: i64) -> Result<ArtistDto> {
    get(db, id)
        .await?
        .ok_or_else(|| MusicError::NotFound(format!("artist {id} not found")))
}

pub async fn by_mbid(db: &Db, mbid: &str) -> Result<Option<ArtistDto>> {
    let row = sqlx::query(&format!(
        "SELECT {COLS} FROM artists a WHERE a.musicbrainz_id = ?"
    ))
    .bind(mbid)
    .fetch_optional(db.pool())
    .await?;
    row.as_ref().map(map).transpose()
}

/// `(id, clean_name)` of every artist.
pub async fn clean_names(db: &Db) -> Result<Vec<(i64, String)>> {
    let rows = sqlx::query("SELECT id, clean_name FROM artists")
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| Ok((r.try_get("id")?, r.try_get("clean_name")?)))
        .collect()
}

/// `(id, mbid)` of every artist.
pub async fn mbids(db: &Db) -> Result<Vec<(i64, String)>> {
    let rows = sqlx::query("SELECT id, musicbrainz_id FROM artists")
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| Ok((r.try_get("id")?, r.try_get("musicbrainz_id")?)))
        .collect()
}

/// Filtered, sorted, paged list.
pub async fn page(db: &Db, q: &ArtistListQuery) -> Result<(Vec<ArtistDto>, u64, u32, u32)> {
    let mut wh = Vec::new();
    if q.search.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        wh.push("(a.name LIKE ? OR a.sort_name LIKE ?)");
    }
    if q.monitored.is_some() {
        wh.push("a.monitored = ?");
    }
    let where_sql = if wh.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", wh.join(" AND "))
    };
    let order = match q.sort.as_deref().unwrap_or("sort_name") {
        "name" => "a.name COLLATE NOCASE",
        "added" | "created_at" => "a.created_at",
        "albums" | "album_count" => "s_albums",
        "tracks" | "track_count" => "s_tracks",
        "size" | "size_on_disk" => "s_size",
        "path" => "a.path",
        "last_refreshed" => "a.last_refreshed",
        _ => "a.sort_name COLLATE NOCASE",
    };
    let dir = match q.dir.unwrap_or_default() {
        SortDir::Asc => "ASC",
        SortDir::Desc => "DESC",
    };
    let (page, size, window) = page_window(q.page, q.page_size);
    let limit = window.map_or(String::new(), |(o, l)| format!(" LIMIT {l} OFFSET {o}"));
    let like = q.search.as_deref().map(|s| format!("%{}%", s.trim()));
    let list_sql = format!("SELECT {COLS} FROM artists a{where_sql} ORDER BY {order} {dir}, a.id{limit}");
    let count_sql = format!("SELECT COUNT(*) AS n FROM artists a{where_sql}");
    let mut lq = sqlx::query(&list_sql);
    let mut cq = sqlx::query(&count_sql);
    if let Some(l) = like.as_ref().filter(|l| l.len() > 2) {
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

/// Columns written on insert/refresh.
#[derive(Debug, Clone)]
pub struct ArtistFields {
    pub musicbrainz_id: String,
    pub name: String,
    pub sort_name: String,
    pub disambiguation: Option<String>,
    pub artist_type: Option<String>,
    pub country: Option<String>,
    pub overview: Option<String>,
    pub genres: Vec<String>,
    pub images: ImagesDto,
    pub links: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NewArtist {
    pub fields: ArtistFields,
    pub monitored: bool,
    pub monitor_new_items: NewAlbumMonitor,
    pub quality_profile_id: i64,
    pub metadata_profile_id: i64,
    pub root_folder_id: i64,
    pub path: String,
    pub tags: Vec<i64>,
    pub added_by_request_id: Option<i64>,
    /// Added with the owner's own details (`POST /customitems`).
    pub custom: bool,
}

pub async fn insert(db: &Db, a: &NewArtist) -> Result<i64> {
    let now = now_ms();
    let f = &a.fields;
    let res = sqlx::query(
        "INSERT INTO artists (musicbrainz_id, name, sort_name, clean_name, disambiguation, artist_type, country,
            overview, genres, images, links, monitored, monitor_new_items, quality_profile_id, metadata_profile_id,
            root_folder_id, path, tags, added_by_request_id, custom, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&f.musicbrainz_id)
    .bind(&f.name)
    .bind(&f.sort_name)
    .bind(crate::convert::clean_name(&f.name))
    .bind(&f.disambiguation)
    .bind(&f.artist_type)
    .bind(&f.country)
    .bind(&f.overview)
    .bind(json(&f.genres))
    .bind(json(&f.images))
    .bind(json(&f.links))
    .bind(a.monitored)
    .bind(enum_str(&a.monitor_new_items))
    .bind(a.quality_profile_id)
    .bind(a.metadata_profile_id)
    .bind(a.root_folder_id)
    .bind(&a.path)
    .bind(json(&a.tags))
    .bind(a.added_by_request_id)
    .bind(a.custom)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            MusicError::Conflict(format!("artist {} is already in the library", f.musicbrainz_id))
        }
        _ => e.into(),
    })?;
    Ok(res.last_insert_rowid())
}

/// Metadata refresh: replaces the descriptive columns and stamps `last_refreshed`.
pub async fn update_fields(db: &Db, id: i64, f: &ArtistFields) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        "UPDATE artists SET name = ?, sort_name = ?, clean_name = ?, disambiguation = ?, artist_type = ?,
            country = ?, overview = COALESCE(?, overview), genres = ?, images = ?, links = ?, last_refreshed = ?,
            updated_at = ? WHERE id = ?",
    )
    .bind(&f.name)
    .bind(&f.sort_name)
    .bind(crate::convert::clean_name(&f.name))
    .bind(&f.disambiguation)
    .bind(&f.artist_type)
    .bind(&f.country)
    .bind(&f.overview)
    .bind(json(&f.genres))
    .bind(json(&f.images))
    .bind(json(&f.links))
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Saves the user-editable columns of `a`.
pub async fn save_settings(db: &Db, a: &ArtistDto) -> Result<()> {
    sqlx::query(
        "UPDATE artists SET monitored = ?, monitor_new_items = ?, quality_profile_id = ?, metadata_profile_id = ?,
            root_folder_id = ?, path = ?, tags = ?, updated_at = ? WHERE id = ?",
    )
    .bind(a.monitored)
    .bind(enum_str(&a.monitor_new_items))
    .bind(a.quality_profile_id)
    .bind(a.metadata_profile_id)
    .bind(a.root_folder_id)
    .bind(&a.path)
    .bind(json(&a.tags))
    .bind(now_ms())
    .bind(a.id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM artists WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Ids of artists last refreshed before `before_ms` (or never).
pub async fn stale(db: &Db, before_ms: i64) -> Result<Vec<i64>> {
    let rows =
        sqlx::query("SELECT id FROM artists WHERE last_refreshed IS NULL OR last_refreshed < ? ORDER BY id")
            .bind(before_ms)
            .fetch_all(db.pool())
            .await?;
    rows.iter().map(|r| Ok(r.try_get("id")?)).collect()
}

/// Artists using a column value (e.g. `root_folder_id`, `metadata_profile_id`).
pub async fn count_using(db: &Db, column: &str, id: i64) -> Result<i64> {
    let column = match column {
        "root_folder_id" => "root_folder_id",
        "metadata_profile_id" => "metadata_profile_id",
        _ => "quality_profile_id",
    };
    let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM artists WHERE {column} = ?"))
        .bind(id)
        .fetch_one(db.pool())
        .await?;
    Ok(n)
}
