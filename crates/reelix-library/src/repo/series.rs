//! `series`, `seasons` and `episodes` rows.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use reelix_api_types::library::{
    EpisodeDto, SeasonDto, SeasonStatistics, SeriesDto, SeriesStatistics, SeriesStatus,
};
use reelix_core::MediaKind;
use reelix_db::{Db, from_ms, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{misc, placeholders};
use crate::convert::{clean_title, date_str, date_utc, enum_str, from_json, json, parse_date, parse_enum};
use crate::error::Result;

const COLS: &str = "id, tmdb_id, tvdb_id, imdb_id, title, original_title, sort_title, year, status, overview, network, \
    series_type, runtime, genres, certification, original_language, images, ratings, first_aired, last_aired, \
    next_airing, monitored, monitor_new_items, season_folder, quality_profile_id, root_folder_id, path, \
    added_by_request_id, last_refreshed, created_at";

fn opt_u32(r: &SqliteRow, col: &str) -> Result<Option<u32>> {
    Ok(r.try_get::<Option<i64>, _>(col)?.map(|v| v as u32))
}

fn map(r: &SqliteRow) -> Result<SeriesDto> {
    Ok(SeriesDto {
        id: r.try_get("id")?,
        tmdb_id: opt_u32(r, "tmdb_id")?,
        tvdb_id: opt_u32(r, "tvdb_id")?,
        imdb_id: r.try_get("imdb_id")?,
        title: r.try_get("title")?,
        original_title: r.try_get("original_title")?,
        sort_title: r.try_get("sort_title")?,
        year: r.try_get::<Option<i64>, _>("year")?.map(|y| y as i32),
        overview: r.try_get("overview")?,
        status: parse_enum(&r.try_get::<String, _>("status")?).unwrap_or(SeriesStatus::Continuing),
        network: r.try_get("network")?,
        series_type: parse_enum(&r.try_get::<String, _>("series_type")?).unwrap_or_default(),
        runtime_minutes: opt_u32(r, "runtime")?,
        genres: from_json(r.try_get("genres")?),
        certification: r.try_get("certification")?,
        original_language: r.try_get("original_language")?,
        images: from_json(r.try_get("images")?),
        ratings: from_json(r.try_get("ratings")?),
        alternative_titles: Vec::new(),
        first_aired: parse_date(r.try_get("first_aired")?),
        last_aired: parse_date(r.try_get("last_aired")?),
        next_airing: parse_date(r.try_get("next_airing")?),
        monitored: r.try_get("monitored")?,
        monitor_new_items: parse_enum(&r.try_get::<String, _>("monitor_new_items")?).unwrap_or_default(),
        season_folder: r.try_get("season_folder")?,
        quality_profile_id: r.try_get("quality_profile_id")?,
        root_folder_id: r.try_get("root_folder_id")?,
        path: r.try_get("path")?,
        tags: Vec::new(),
        added_by_request_id: r.try_get("added_by_request_id")?,
        seasons: Vec::new(),
        statistics: SeriesStatistics::default(),
        cast: Vec::new(),
        added: from_ms(r.try_get("created_at")?),
        last_refreshed: r.try_get::<Option<i64>, _>("last_refreshed")?.map(from_ms),
    })
}

/// Minimal episode facts for statistics.
struct EpFact {
    series_id: i64,
    season: u32,
    air_date: Option<NaiveDate>,
    monitored: bool,
    file_id: Option<i64>,
}

/// Series (all, or `ids`) with seasons, statistics, tags and alternative titles.
/// Every series with tags and alternative titles but no seasons or statistics, for filtering
/// and sorting a list; load the page itself with [`hydrate`].
pub async fn load_light(db: &Db) -> Result<Vec<SeriesDto>> {
    let sql = format!("SELECT {COLS} FROM series ORDER BY sort_title");
    let mut list: Vec<SeriesDto> = sqlx::query(&sql)
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map)
        .collect::<Result<_>>()?;
    let mut tags = misc::tags_of(db, MediaKind::Series, None).await?;
    let mut alts = misc::alt_titles_of(db, MediaKind::Series, None).await?;
    for s in &mut list {
        s.tags = tags.remove(&s.id).unwrap_or_default();
        s.alternative_titles = alts.remove(&s.id).unwrap_or_default();
    }
    Ok(list)
}

/// Replaces light rows with fully loaded ones, keeping their order.
pub async fn hydrate(db: &Db, light: Vec<SeriesDto>, today: NaiveDate) -> Result<Vec<SeriesDto>> {
    if light.is_empty() {
        return Ok(light);
    }
    let ids: Vec<i64> = light.iter().map(|s| s.id).collect();
    let mut full: HashMap<i64, SeriesDto> = load(db, Some(&ids), today)
        .await?
        .into_iter()
        .map(|s| (s.id, s))
        .collect();
    Ok(ids.iter().filter_map(|id| full.remove(id)).collect())
}

pub async fn load(db: &Db, ids: Option<&[i64]>, today: NaiveDate) -> Result<Vec<SeriesDto>> {
    let sql = match ids {
        Some(ids) => format!(
            "SELECT {COLS} FROM series WHERE id IN ({}) ORDER BY sort_title",
            placeholders(ids.len())
        ),
        None => format!("SELECT {COLS} FROM series ORDER BY sort_title"),
    };
    let mut q = sqlx::query(&sql);
    for id in ids.unwrap_or_default() {
        q = q.bind(id);
    }
    let mut list: Vec<SeriesDto> = q
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map)
        .collect::<Result<_>>()?;
    if list.is_empty() {
        return Ok(list);
    }
    let series_ids: Vec<i64> = list.iter().map(|s| s.id).collect();
    let filter = ids.map(|_| series_ids.as_slice());

    // Seasons.
    let season_sql = match filter {
        Some(f) => format!(
            "SELECT series_id, season_number, monitored, title, overview, poster, air_date FROM seasons
             WHERE series_id IN ({}) ORDER BY season_number",
            placeholders(f.len())
        ),
        None => "SELECT series_id, season_number, monitored, title, overview, poster, air_date FROM seasons ORDER BY season_number".into(),
    };
    let mut q = sqlx::query(&season_sql);
    for id in filter.unwrap_or_default() {
        q = q.bind(id);
    }
    let mut seasons: HashMap<i64, Vec<SeasonDto>> = HashMap::new();
    for r in q.fetch_all(db.pool()).await? {
        seasons
            .entry(r.try_get("series_id")?)
            .or_default()
            .push(SeasonDto {
                season_number: r.try_get::<i64, _>("season_number")? as u32,
                monitored: r.try_get("monitored")?,
                title: r.try_get("title")?,
                overview: r.try_get("overview")?,
                poster: r.try_get("poster")?,
                air_date: parse_date(r.try_get("air_date")?),
                statistics: SeasonStatistics::default(),
            });
    }

    // Episode facts + file sizes.
    let ep_sql = match filter {
        Some(f) => format!(
            "SELECT series_id, season_number, air_date, monitored, episode_file_id FROM episodes WHERE series_id IN ({})",
            placeholders(f.len())
        ),
        None => "SELECT series_id, season_number, air_date, monitored, episode_file_id FROM episodes".into(),
    };
    let mut q = sqlx::query(&ep_sql);
    for id in filter.unwrap_or_default() {
        q = q.bind(id);
    }
    let facts: Vec<EpFact> = q
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(|r| {
            Ok(EpFact {
                series_id: r.try_get("series_id")?,
                season: r.try_get::<i64, _>("season_number")? as u32,
                air_date: parse_date(r.try_get("air_date")?),
                monitored: r.try_get("monitored")?,
                file_id: r.try_get("episode_file_id")?,
            })
        })
        .collect::<Result<_>>()?;
    // Sizes only: building full file rows (quality/format JSON) for every file is wasted here.
    let size_sql = match filter {
        Some(f) => format!(
            "SELECT id, size FROM episode_files WHERE series_id IN ({})",
            placeholders(f.len())
        ),
        None => "SELECT id, size FROM episode_files".into(),
    };
    let mut q = sqlx::query(&size_sql);
    for id in filter.unwrap_or_default() {
        q = q.bind(id);
    }
    let file_sizes: HashMap<i64, u64> = q
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(|r| {
            Ok((
                r.try_get::<i64, _>("id")?,
                r.try_get::<i64, _>("size")?.max(0) as u64,
            ))
        })
        .collect::<Result<_>>()?;

    let mut stats: HashMap<(i64, u32), (SeasonStatistics, std::collections::HashSet<i64>)> = HashMap::new();
    for e in &facts {
        let (s, seen) = stats.entry((e.series_id, e.season)).or_default();
        s.total_episode_count += 1;
        let aired = e.air_date.is_some_and(|d| d <= today);
        if e.monitored {
            s.monitored_episode_count += 1;
        }
        if e.file_id.is_some() || (e.monitored && aired) {
            s.episode_count += 1;
        }
        if let Some(fid) = e.file_id {
            s.episode_file_count += 1;
            if seen.insert(fid) {
                s.size_on_disk += file_sizes.get(&fid).copied().unwrap_or(0);
            }
        }
        if let Some(d) = e.air_date {
            if d > today {
                s.next_airing = Some(s.next_airing.map_or(d, |n| n.min(d)));
            } else {
                s.previous_airing = Some(s.previous_airing.map_or(d, |p| p.max(d)));
            }
        }
    }

    let mut tags = misc::tags_of(db, MediaKind::Series, ids).await?;
    let mut alts = misc::alt_titles_of(db, MediaKind::Series, ids).await?;
    for s in &mut list {
        s.tags = tags.remove(&s.id).unwrap_or_default();
        s.alternative_titles = alts.remove(&s.id).unwrap_or_default();
        let mut ss = seasons.remove(&s.id).unwrap_or_default();
        let mut total = SeriesStatistics::default();
        for season in &mut ss {
            if let Some((st, _)) = stats.remove(&(s.id, season.season_number)) {
                season.statistics = st;
            }
            let st = &season.statistics;
            if season.season_number > 0 {
                total.season_count += 1;
            }
            total.episode_count += st.episode_count;
            total.episode_file_count += st.episode_file_count;
            total.total_episode_count += st.total_episode_count;
            total.monitored_episode_count += st.monitored_episode_count;
            total.size_on_disk += st.size_on_disk;
        }
        total.percent_of_episodes = if total.episode_count == 0 {
            100.0
        } else {
            (f64::from(total.episode_file_count) / f64::from(total.episode_count) * 100.0).min(100.0)
        };
        s.statistics = total;
        s.seasons = ss;
    }
    Ok(list)
}

pub async fn id_by_tmdb(db: &Db, tmdb_id: u32) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM series WHERE tmdb_id = ?")
        .bind(tmdb_id as i64)
        .fetch_optional(db.pool())
        .await?)
}

pub async fn id_by_tvdb(db: &Db, tvdb_id: u32) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM series WHERE tvdb_id = ?")
        .bind(tvdb_id as i64)
        .fetch_optional(db.pool())
        .await?)
}

/// `tmdb_id → series id` and `tvdb_id → series id`.
pub async fn id_index(db: &Db) -> Result<(HashMap<u32, i64>, HashMap<u32, i64>)> {
    let rows = sqlx::query("SELECT id, tmdb_id, tvdb_id FROM series")
        .fetch_all(db.pool())
        .await?;
    let (mut tmdb, mut tvdb) = (HashMap::new(), HashMap::new());
    for r in rows {
        let id: i64 = r.try_get("id")?;
        tmdb.insert(r.try_get::<i64, _>("tmdb_id")? as u32, id);
        if let Some(t) = r.try_get::<Option<i64>, _>("tvdb_id")? {
            tvdb.insert(t as u32, id);
        }
    }
    Ok((tmdb, tvdb))
}

pub async fn insert(db: &Db, s: &SeriesDto) -> Result<i64> {
    let now = now_ms();
    let tmdb = s.tmdb_id.unwrap_or_default();
    let res = sqlx::query(
        "INSERT INTO series (tmdb_id, title, sort_title, clean_title, quality_profile_id, root_folder_id, path,
             created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(tmdb as i64)
    .bind(&s.title)
    .bind(&s.sort_title)
    .bind(clean_title(&s.title))
    .bind(s.quality_profile_id)
    .bind(s.root_folder_id)
    .bind(&s.path)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            crate::LibraryError::Conflict(format!("series with TMDB id {tmdb} is already in the library"))
        }
        _ => e.into(),
    })?;
    let id = res.last_insert_rowid();
    save(db, id, s).await?;
    Ok(id)
}

/// Writes the stored series columns and tags (not seasons/episodes/credits).
pub async fn save(db: &Db, id: i64, s: &SeriesDto) -> Result<()> {
    sqlx::query(
        "UPDATE series SET tvdb_id = ?, imdb_id = ?, title = ?, original_title = ?, sort_title = ?, clean_title = ?,
             year = ?, status = ?, overview = ?, network = ?, series_type = ?, runtime = ?, genres = ?,
             certification = ?, original_language = ?, images = ?, ratings = ?, first_aired = ?, last_aired = ?,
             next_airing = ?, monitored = ?, monitor_new_items = ?, season_folder = ?, quality_profile_id = ?,
             root_folder_id = ?, path = ?, added_by_request_id = ?, last_refreshed = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(s.tvdb_id.map(i64::from))
    .bind(&s.imdb_id)
    .bind(&s.title)
    .bind(&s.original_title)
    .bind(&s.sort_title)
    .bind(clean_title(&s.title))
    .bind(s.year)
    .bind(enum_str(&s.status))
    .bind(&s.overview)
    .bind(&s.network)
    .bind(enum_str(&s.series_type))
    .bind(s.runtime_minutes.map(i64::from))
    .bind(json(&s.genres))
    .bind(&s.certification)
    .bind(&s.original_language)
    .bind(json(&s.images))
    .bind(json(&s.ratings))
    .bind(date_str(s.first_aired))
    .bind(date_str(s.last_aired))
    .bind(date_str(s.next_airing))
    .bind(s.monitored)
    .bind(enum_str(&s.monitor_new_items))
    .bind(s.season_folder)
    .bind(s.quality_profile_id)
    .bind(s.root_folder_id)
    .bind(&s.path)
    .bind(s.added_by_request_id)
    .bind(s.last_refreshed.map(|d| d.timestamp_millis()))
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    misc::set_tags(db, MediaKind::Series, id, &s.tags).await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM series WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    misc::delete_media_extras(db, MediaKind::Series, id).await?;
    Ok(res.rows_affected() > 0)
}

pub async fn all_ids(db: &Db) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM series ORDER BY id")
        .fetch_all(db.pool())
        .await?)
}

// ---------------- seasons ----------------

/// Inserts a season (with `monitored`) or updates its metadata (keeping `monitored`).
/// Returns true when the season is new.
pub async fn upsert_season(db: &Db, series_id: i64, s: &SeasonDto) -> Result<bool> {
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT id FROM seasons WHERE series_id = ? AND season_number = ?")
            .bind(series_id)
            .bind(s.season_number as i64)
            .fetch_optional(db.pool())
            .await?;
    if let Some(id) = exists {
        sqlx::query("UPDATE seasons SET title = ?, overview = ?, poster = ?, air_date = ? WHERE id = ?")
            .bind(&s.title)
            .bind(&s.overview)
            .bind(&s.poster)
            .bind(date_str(s.air_date))
            .bind(id)
            .execute(db.pool())
            .await?;
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO seasons (series_id, season_number, monitored, title, overview, poster, air_date) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(series_id)
    .bind(s.season_number as i64)
    .bind(s.monitored)
    .bind(&s.title)
    .bind(&s.overview)
    .bind(&s.poster)
    .bind(date_str(s.air_date))
    .execute(db.pool())
    .await?;
    Ok(true)
}

pub async fn set_season_monitored(db: &Db, series_id: i64, season: u32, monitored: bool) -> Result<bool> {
    let res = sqlx::query("UPDATE seasons SET monitored = ? WHERE series_id = ? AND season_number = ?")
        .bind(monitored)
        .bind(series_id)
        .bind(season as i64)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn delete_seasons_except(db: &Db, series_id: i64, keep: &[u32]) -> Result<()> {
    let sql = format!(
        "DELETE FROM seasons WHERE series_id = ? AND season_number NOT IN ({})",
        placeholders(keep.len())
    );
    let mut q = sqlx::query(&sql).bind(series_id);
    for k in keep {
        q = q.bind(*k as i64);
    }
    q.execute(db.pool()).await?;
    Ok(())
}

// ---------------- episodes ----------------

const EP_COLS: &str = "id, series_id, season_number, episode_number, absolute_number, scene_season_number, \
    scene_episode_number, scene_absolute_number, title, overview, air_date, runtime, still, finale, monitored, \
    episode_file_id";

fn map_episode(r: &SqliteRow) -> Result<EpisodeDto> {
    let air_date = parse_date(r.try_get("air_date")?);
    let file: Option<i64> = r.try_get("episode_file_id")?;
    Ok(EpisodeDto {
        id: r.try_get("id")?,
        series_id: r.try_get("series_id")?,
        season_number: r.try_get::<i64, _>("season_number")? as u32,
        episode_number: r.try_get::<i64, _>("episode_number")? as u32,
        absolute_number: opt_u32(r, "absolute_number")?,
        scene_season_number: opt_u32(r, "scene_season_number")?,
        scene_episode_number: opt_u32(r, "scene_episode_number")?,
        scene_absolute_number: opt_u32(r, "scene_absolute_number")?,
        title: r.try_get("title")?,
        overview: r.try_get("overview")?,
        air_date,
        air_date_utc: air_date.map(date_utc),
        runtime_minutes: opt_u32(r, "runtime")?,
        still: r.try_get("still")?,
        finale: r.try_get("finale")?,
        monitored: r.try_get("monitored")?,
        has_file: file.is_some(),
        episode_file_id: file,
        episode_file: None,
    })
}

/// Episodes matching a SQL condition (`where_sql` uses `?` placeholders bound from `binds`).
pub async fn episodes_where(db: &Db, where_sql: &str, binds: &[i64]) -> Result<Vec<EpisodeDto>> {
    let sql = format!(
        "SELECT {EP_COLS} FROM episodes WHERE {where_sql} ORDER BY series_id, season_number, episode_number"
    );
    let mut q = sqlx::query(&sql);
    for b in binds {
        q = q.bind(b);
    }
    q.fetch_all(db.pool()).await?.iter().map(map_episode).collect()
}

pub async fn episodes(db: &Db, series_id: i64, season: Option<u32>) -> Result<Vec<EpisodeDto>> {
    match season {
        Some(s) => episodes_where(db, "series_id = ? AND season_number = ?", &[series_id, s as i64]).await,
        None => episodes_where(db, "series_id = ?", &[series_id]).await,
    }
}

pub async fn episodes_by_ids(db: &Db, ids: &[i64]) -> Result<Vec<EpisodeDto>> {
    episodes_where(db, &format!("id IN ({})", placeholders(ids.len())), ids).await
}

/// One page of wanted/missing episodes, filtered, sorted and paged in SQL; returns the page
/// and the total. `sort` is `date` (default), `title` or `added`.
pub async fn missing_episodes_page(
    db: &Db,
    today: NaiveDate,
    only_monitored: bool,
    sort: &str,
    desc: bool,
    limit: Option<u32>,
    offset: u64,
) -> Result<(Vec<EpisodeDto>, u64)> {
    let filter = format!(
        "e.season_number > 0 AND e.episode_file_id IS NULL AND e.air_date IS NOT NULL AND e.air_date <= ?{}",
        if only_monitored {
            " AND e.monitored = 1 AND s.monitored = 1"
        } else {
            ""
        }
    );
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM episodes e JOIN series s ON s.id = e.series_id WHERE {filter}"
    ))
    .bind(date_str(Some(today)))
    .fetch_one(db.pool())
    .await?;
    let dir = if desc { "DESC" } else { "ASC" };
    let key = match sort {
        "title" => format!("s.title COLLATE NOCASE {dir}"),
        "added" => format!("s.created_at {dir}"),
        _ => format!("e.air_date {dir}"),
    };
    let cols = EP_COLS
        .split(',')
        .map(|c| format!("e.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {cols} FROM episodes e JOIN series s ON s.id = e.series_id WHERE {filter}
         ORDER BY {key}, s.title {dir}, e.season_number {dir}, e.episode_number {dir}
         LIMIT ? OFFSET ?"
    );
    let rows = sqlx::query(&sql)
        .bind(date_str(Some(today)))
        .bind(limit.map_or(-1, i64::from))
        .bind(offset as i64)
        .fetch_all(db.pool())
        .await?;
    Ok((
        rows.iter().map(map_episode).collect::<Result<_>>()?,
        total.max(0) as u64,
    ))
}

/// Episodes that have a file, grouped by series, in one query.
pub async fn episodes_with_files(db: &Db) -> Result<HashMap<i64, Vec<EpisodeDto>>> {
    let mut out: HashMap<i64, Vec<EpisodeDto>> = HashMap::new();
    for e in episodes_where(db, "episode_file_id IS NOT NULL", &[]).await? {
        out.entry(e.series_id).or_default().push(e);
    }
    Ok(out)
}

/// Episodes airing in `[start, end]` (dates as text compare correctly).
pub async fn episodes_between(db: &Db, start: NaiveDate, end: NaiveDate) -> Result<Vec<EpisodeDto>> {
    let sql =
        format!("SELECT {EP_COLS} FROM episodes WHERE air_date >= ? AND air_date <= ? ORDER BY air_date");
    sqlx::query(&sql)
        .bind(date_str(Some(start)))
        .bind(date_str(Some(end)))
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map_episode)
        .collect()
}

/// Inserts or updates an episode's metadata (keeping `monitored` and file); returns (id, inserted).
pub async fn upsert_episode(db: &Db, e: &EpisodeDto) -> Result<(i64, bool)> {
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM episodes WHERE series_id = ? AND season_number = ? AND episode_number = ?",
    )
    .bind(e.series_id)
    .bind(e.season_number as i64)
    .bind(e.episode_number as i64)
    .fetch_optional(db.pool())
    .await?;
    let now = now_ms();
    if let Some(id) = existing {
        sqlx::query(
            "UPDATE episodes SET absolute_number = ?, title = ?, overview = ?, air_date = ?, runtime = ?, still = ?,
                 finale = ?, updated_at = ? WHERE id = ?",
        )
        .bind(e.absolute_number.map(i64::from))
        .bind(&e.title)
        .bind(&e.overview)
        .bind(date_str(e.air_date))
        .bind(e.runtime_minutes.map(i64::from))
        .bind(&e.still)
        .bind(&e.finale)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
        return Ok((id, false));
    }
    let res = sqlx::query(
        "INSERT INTO episodes (series_id, season_number, episode_number, absolute_number, title, overview, air_date,
             runtime, still, finale, monitored, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(e.series_id)
    .bind(e.season_number as i64)
    .bind(e.episode_number as i64)
    .bind(e.absolute_number.map(i64::from))
    .bind(&e.title)
    .bind(&e.overview)
    .bind(date_str(e.air_date))
    .bind(e.runtime_minutes.map(i64::from))
    .bind(&e.still)
    .bind(&e.finale)
    .bind(e.monitored)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok((res.last_insert_rowid(), true))
}

pub async fn delete_episodes(db: &Db, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let sql = format!("DELETE FROM episodes WHERE id IN ({})", placeholders(ids.len()));
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id);
    }
    q.execute(db.pool()).await?;
    Ok(())
}

pub async fn set_episodes_monitored(db: &Db, ids: &[i64], monitored: bool) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "UPDATE episodes SET monitored = ?, updated_at = ? WHERE id IN ({})",
        placeholders(ids.len())
    );
    let mut q = sqlx::query(&sql).bind(monitored).bind(now_ms());
    for id in ids {
        q = q.bind(id);
    }
    Ok(q.execute(db.pool()).await?.rows_affected())
}

pub async fn set_episode_file(db: &Db, ids: &[i64], file_id: Option<i64>) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "UPDATE episodes SET episode_file_id = ?, updated_at = ? WHERE id IN ({})",
        placeholders(ids.len())
    );
    let mut q = sqlx::query(&sql).bind(file_id).bind(now_ms());
    for id in ids {
        q = q.bind(id);
    }
    q.execute(db.pool()).await?;
    Ok(())
}

/// Episode ids per episode file of a series.
pub async fn file_episode_map(db: &Db, series_id: Option<i64>) -> Result<BTreeMap<i64, Vec<i64>>> {
    let rows = match series_id {
        Some(s) => {
            sqlx::query("SELECT id, episode_file_id FROM episodes WHERE series_id = ? AND episode_file_id IS NOT NULL ORDER BY season_number, episode_number")
                .bind(s)
                .fetch_all(db.pool())
                .await?
        }
        None => {
            sqlx::query("SELECT id, episode_file_id FROM episodes WHERE episode_file_id IS NOT NULL ORDER BY season_number, episode_number")
                .fetch_all(db.pool())
                .await?
        }
    };
    let mut out: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for r in rows {
        out.entry(r.try_get("episode_file_id")?)
            .or_default()
            .push(r.try_get("id")?);
    }
    Ok(out)
}
