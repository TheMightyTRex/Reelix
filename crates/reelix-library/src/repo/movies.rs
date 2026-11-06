//! `movies` rows ⇄ [`MovieDto`].

use std::collections::HashMap;

use chrono::NaiveDate;
use reelix_api_types::library::{CollectionRefDto, MovieDto, MovieFileDto, MovieStatus};
use reelix_core::MediaKind;
use reelix_db::{Db, from_ms, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::files::{self, FileTable};
use super::{misc, placeholders};
use crate::convert::{
    clean_title, date_str, enum_str, from_json, is_available, json, parse_date, parse_enum,
};
use crate::error::Result;
use crate::service::ProfileCache;

const COLS: &str = "m.id, m.tmdb_id, m.imdb_id, m.title, m.original_title, m.sort_title, m.year, m.status, m.overview, \
    m.tagline, m.runtime, m.studio, m.genres, m.certification, m.original_language, m.images, m.ratings, m.in_cinemas, \
    m.digital_release, m.physical_release, m.youtube_trailer, m.homepage, m.monitored, m.minimum_availability, \
    m.quality_profile_id, m.root_folder_id, m.path, m.collection_id, m.added_by_request_id, m.last_refreshed, \
    m.created_at, c.tmdb_id AS c_tmdb, c.title AS c_title";

fn map(r: &SqliteRow) -> Result<MovieDto> {
    let status: MovieStatus = parse_enum(&r.try_get::<String, _>("status")?).unwrap_or(MovieStatus::Tba);
    let min = parse_enum(&r.try_get::<String, _>("minimum_availability")?).unwrap_or_default();
    let collection = match r.try_get::<Option<i64>, _>("collection_id")? {
        Some(id) => Some(CollectionRefDto {
            id,
            tmdb_id: r.try_get::<Option<i64>, _>("c_tmdb")?.unwrap_or_default() as u32,
            title: r.try_get::<Option<String>, _>("c_title")?.unwrap_or_default(),
        }),
        None => None,
    };
    Ok(MovieDto {
        id: r.try_get("id")?,
        tmdb_id: r.try_get::<i64, _>("tmdb_id")? as u32,
        imdb_id: r.try_get("imdb_id")?,
        title: r.try_get("title")?,
        original_title: r.try_get("original_title")?,
        sort_title: r.try_get("sort_title")?,
        year: r.try_get::<Option<i64>, _>("year")?.map(|y| y as i32),
        overview: r.try_get("overview")?,
        tagline: r.try_get("tagline")?,
        status,
        runtime_minutes: r.try_get::<Option<i64>, _>("runtime")?.map(|v| v as u32),
        studio: r.try_get("studio")?,
        genres: from_json(r.try_get("genres")?),
        certification: r.try_get("certification")?,
        original_language: r.try_get("original_language")?,
        images: from_json(r.try_get("images")?),
        ratings: from_json(r.try_get("ratings")?),
        alternative_titles: Vec::new(),
        in_cinemas: parse_date(r.try_get("in_cinemas")?),
        digital_release: parse_date(r.try_get("digital_release")?),
        physical_release: parse_date(r.try_get("physical_release")?),
        youtube_trailer: r.try_get("youtube_trailer")?,
        homepage: r.try_get("homepage")?,
        monitored: r.try_get("monitored")?,
        minimum_availability: min,
        is_available: is_available(status, min),
        quality_profile_id: r.try_get("quality_profile_id")?,
        root_folder_id: r.try_get("root_folder_id")?,
        path: r.try_get("path")?,
        collection,
        tags: Vec::new(),
        added_by_request_id: r.try_get("added_by_request_id")?,
        has_file: false,
        size_on_disk: 0,
        movie_file: None,
        cast: Vec::new(),
        added: from_ms(r.try_get("created_at")?),
        last_refreshed: r.try_get::<Option<i64>, _>("last_refreshed")?.map(from_ms),
    })
}

/// Columns list filtering/sorting and the wanted/calendar views need (no overview, credits,
/// links…): decoding 10k full rows is most of a list request's cost.
const LIGHT_COLS: &str = "m.id, m.tmdb_id, m.title, m.original_title, m.sort_title, m.year, m.status, m.runtime, \
    m.genres, m.images, m.ratings, m.in_cinemas, m.digital_release, m.physical_release, m.monitored, \
    m.minimum_availability, m.quality_profile_id, m.root_folder_id, m.path, m.collection_id, m.created_at";

fn map_light(r: &SqliteRow) -> Result<MovieDto> {
    let status: MovieStatus = parse_enum(&r.try_get::<String, _>("status")?).unwrap_or(MovieStatus::Tba);
    let min = parse_enum(&r.try_get::<String, _>("minimum_availability")?).unwrap_or_default();
    Ok(MovieDto {
        id: r.try_get("id")?,
        tmdb_id: r.try_get::<i64, _>("tmdb_id")? as u32,
        imdb_id: None,
        title: r.try_get("title")?,
        original_title: r.try_get("original_title")?,
        sort_title: r.try_get("sort_title")?,
        year: r.try_get::<Option<i64>, _>("year")?.map(|y| y as i32),
        overview: None,
        tagline: None,
        status,
        runtime_minutes: r.try_get::<Option<i64>, _>("runtime")?.map(|v| v as u32),
        studio: None,
        genres: from_json(r.try_get("genres")?),
        certification: None,
        original_language: None,
        images: from_json(r.try_get("images")?),
        ratings: from_json(r.try_get("ratings")?),
        alternative_titles: Vec::new(),
        in_cinemas: parse_date(r.try_get("in_cinemas")?),
        digital_release: parse_date(r.try_get("digital_release")?),
        physical_release: parse_date(r.try_get("physical_release")?),
        youtube_trailer: None,
        homepage: None,
        monitored: r.try_get("monitored")?,
        minimum_availability: min,
        is_available: is_available(status, min),
        quality_profile_id: r.try_get("quality_profile_id")?,
        root_folder_id: r.try_get("root_folder_id")?,
        path: r.try_get("path")?,
        // Only the id matters for filtering; the page is reloaded in full.
        collection: r
            .try_get::<Option<i64>, _>("collection_id")?
            .map(|id| CollectionRefDto {
                id,
                tmdb_id: 0,
                title: String::new(),
            }),
        tags: Vec::new(),
        added_by_request_id: None,
        has_file: false,
        size_on_disk: 0,
        movie_file: None,
        cast: Vec::new(),
        added: from_ms(r.try_get("created_at")?),
        last_refreshed: None,
    })
}

/// Movies (all, or `ids`) with tags, alternative titles and their primary file.
pub async fn load(db: &Db, ids: Option<&[i64]>, profiles: &ProfileCache) -> Result<Vec<MovieDto>> {
    let base = format!("SELECT {COLS} FROM movies m LEFT JOIN collections c ON c.id = m.collection_id");
    let sql = match ids {
        Some(ids) => format!(
            "{base} WHERE m.id IN ({}) ORDER BY m.sort_title",
            placeholders(ids.len())
        ),
        None => format!("{base} ORDER BY m.sort_title"),
    };
    let mut q = sqlx::query(&sql);
    for id in ids.unwrap_or_default() {
        q = q.bind(id);
    }
    let mut movies: Vec<MovieDto> = q
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map)
        .collect::<Result<_>>()?;
    if movies.is_empty() {
        return Ok(movies);
    }
    let owner_ids: Vec<i64> = movies.iter().map(|m| m.id).collect();
    let scope = ids.map(|_| owner_ids.as_slice());
    let mut tags = misc::tags_of(db, MediaKind::Movie, scope).await?;
    let mut alts = misc::alt_titles_of(db, MediaKind::Movie, scope).await?;
    let file_rows = files::list(db, FileTable::Movie, ids.map(|_| owner_ids.as_slice())).await?;
    let mut by_movie: HashMap<i64, Vec<files::FileRow>> = HashMap::new();
    for f in file_rows {
        by_movie.entry(f.owner_id).or_default().push(f);
    }
    for m in &mut movies {
        m.tags = tags.remove(&m.id).unwrap_or_default();
        m.alternative_titles = alts.remove(&m.id).unwrap_or_default();
        if let Some(fs) = by_movie.remove(&m.id) {
            m.size_on_disk = fs.iter().map(|f| f.size).sum();
            m.has_file = !fs.is_empty();
            let profile = profiles.profile(m.quality_profile_id);
            m.movie_file = fs.last().map(|f| MovieFileDto {
                movie_id: m.id,
                file: f.to_dto(&m.path, profile, &profiles.format_names),
            });
        }
    }
    Ok(movies)
}

/// Every movie with the fields lists filter and sort on (tags, alternative titles,
/// `has_file`, `size_on_disk`) but without file details: cheap enough to run per page
/// request. Load the page itself with [`load`].
pub async fn load_light(db: &Db) -> Result<Vec<MovieDto>> {
    let sql = format!("SELECT {LIGHT_COLS} FROM movies m ORDER BY m.sort_title");
    let mut movies: Vec<MovieDto> = sqlx::query(&sql)
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map_light)
        .collect::<Result<_>>()?;
    let sizes: HashMap<i64, i64> =
        sqlx::query("SELECT movie_id, SUM(size) AS size FROM movie_files GROUP BY movie_id")
            .fetch_all(db.pool())
            .await?
            .iter()
            .map(|r| {
                Ok((
                    r.try_get("movie_id")?,
                    r.try_get::<Option<i64>, _>("size")?.unwrap_or(0),
                ))
            })
            .collect::<Result<_>>()?;
    let mut tags = misc::tags_of(db, MediaKind::Movie, None).await?;
    let mut alts = misc::alt_titles_of(db, MediaKind::Movie, None).await?;
    for m in &mut movies {
        m.tags = tags.remove(&m.id).unwrap_or_default();
        m.alternative_titles = alts.remove(&m.id).unwrap_or_default();
        if let Some(size) = sizes.get(&m.id) {
            m.has_file = true;
            m.size_on_disk = *size as u64;
        }
    }
    Ok(movies)
}

/// Ids of movies with a cinema, digital or physical release in `[start, end]`.
pub async fn released_between(
    db: &Db,
    start: NaiveDate,
    end: NaiveDate,
    unmonitored: bool,
) -> Result<Vec<i64>> {
    let (a, b) = (date_str(Some(start)), date_str(Some(end)));
    let sql = format!(
        "SELECT id FROM movies WHERE ((in_cinemas >= ?1 AND in_cinemas <= ?2)
            OR (digital_release >= ?1 AND digital_release <= ?2)
            OR (physical_release >= ?1 AND physical_release <= ?2)){}",
        if unmonitored { "" } else { " AND monitored = 1" }
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(a)
        .bind(b)
        .fetch_all(db.pool())
        .await?)
}

/// Replaces light rows with fully loaded ones, keeping their order.
pub async fn hydrate(db: &Db, light: Vec<MovieDto>, profiles: &ProfileCache) -> Result<Vec<MovieDto>> {
    if light.is_empty() {
        return Ok(light);
    }
    let ids: Vec<i64> = light.iter().map(|m| m.id).collect();
    let mut full: HashMap<i64, MovieDto> = load(db, Some(&ids), profiles)
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    Ok(ids.iter().filter_map(|id| full.remove(id)).collect())
}

pub async fn get(db: &Db, id: i64, profiles: &ProfileCache) -> Result<Option<MovieDto>> {
    Ok(load(db, Some(&[id]), profiles).await?.pop())
}

pub async fn id_by_tmdb(db: &Db, tmdb_id: u32) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM movies WHERE tmdb_id = ?")
        .bind(tmdb_id as i64)
        .fetch_optional(db.pool())
        .await?)
}

/// `tmdb_id → movie id` for every movie.
pub async fn tmdb_index(db: &Db) -> Result<HashMap<u32, i64>> {
    let rows = sqlx::query("SELECT id, tmdb_id FROM movies")
        .fetch_all(db.pool())
        .await?;
    let mut out = HashMap::new();
    for r in rows {
        out.insert(r.try_get::<i64, _>("tmdb_id")? as u32, r.try_get("id")?);
    }
    Ok(out)
}

/// Inserts a movie (the DTO's `id`, files and computed fields are ignored); returns the id.
pub async fn insert(db: &Db, m: &MovieDto) -> Result<i64> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO movies (tmdb_id, title, sort_title, clean_title, quality_profile_id, root_folder_id, path,
             created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(m.tmdb_id as i64)
    .bind(&m.title)
    .bind(&m.sort_title)
    .bind(clean_title(&m.title))
    .bind(m.quality_profile_id)
    .bind(m.root_folder_id)
    .bind(&m.path)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            crate::LibraryError::Conflict(format!("movie with TMDB id {} is already in the library", m.tmdb_id))
        }
        _ => e.into(),
    })?;
    let id = res.last_insert_rowid();
    save(db, id, m).await?;
    Ok(id)
}

/// Writes every stored column of `m` (not files/credits) to movie `id`, plus tags.
pub async fn save(db: &Db, id: i64, m: &MovieDto) -> Result<()> {
    sqlx::query(
        "UPDATE movies SET imdb_id = ?, title = ?, original_title = ?, sort_title = ?, clean_title = ?, year = ?,
             status = ?, overview = ?, tagline = ?, runtime = ?, studio = ?, genres = ?, certification = ?,
             original_language = ?, images = ?, ratings = ?, in_cinemas = ?, digital_release = ?,
             physical_release = ?, youtube_trailer = ?, homepage = ?, monitored = ?, minimum_availability = ?,
             quality_profile_id = ?, root_folder_id = ?, path = ?, collection_id = ?, added_by_request_id = ?,
             last_refreshed = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(&m.imdb_id)
    .bind(&m.title)
    .bind(&m.original_title)
    .bind(&m.sort_title)
    .bind(clean_title(&m.title))
    .bind(m.year)
    .bind(enum_str(&m.status))
    .bind(&m.overview)
    .bind(&m.tagline)
    .bind(m.runtime_minutes.map(i64::from))
    .bind(&m.studio)
    .bind(json(&m.genres))
    .bind(&m.certification)
    .bind(&m.original_language)
    .bind(json(&m.images))
    .bind(json(&m.ratings))
    .bind(date_str(m.in_cinemas))
    .bind(date_str(m.digital_release))
    .bind(date_str(m.physical_release))
    .bind(&m.youtube_trailer)
    .bind(&m.homepage)
    .bind(m.monitored)
    .bind(enum_str(&m.minimum_availability))
    .bind(m.quality_profile_id)
    .bind(m.root_folder_id)
    .bind(&m.path)
    .bind(m.collection.as_ref().map(|c| c.id))
    .bind(m.added_by_request_id)
    .bind(m.last_refreshed.map(|d| d.timestamp_millis()))
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    misc::set_tags(db, MediaKind::Movie, id, &m.tags).await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM movies WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    misc::delete_media_extras(db, MediaKind::Movie, id).await?;
    Ok(res.rows_affected() > 0)
}

pub async fn all_ids(db: &Db) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM movies ORDER BY id")
        .fetch_all(db.pool())
        .await?)
}
