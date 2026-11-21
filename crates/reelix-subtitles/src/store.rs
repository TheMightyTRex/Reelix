//! Database access: the video files to find subtitles for, what was fetched, and when each file
//! was last searched.

use std::path::{Path, PathBuf};

use chrono::TimeZone;
use reelix_api_types::subtitles::{SubtitleFileDto, SubtitleMediaKind};
use reelix_db::Db;

use crate::error::Result;
use crate::matching::MatchLevel;
use crate::naming::Variant;

/// A video file with what the sources need to find its subtitles.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoFile {
    pub kind: SubtitleMediaKind,
    pub id: i64,
    pub path: PathBuf,
    pub size: u64,
    /// Release name: the scene name, else the downloaded file's name, else the file's own.
    pub release: Option<String>,
    /// Length in seconds, from the media details.
    pub duration: Option<f64>,
    /// Film's id, or the series' for an episode.
    pub imdb_id: Option<u64>,
    pub tmdb_id: Option<u64>,
    pub title: String,
    pub year: Option<i32>,
    pub episode: Option<(u32, u32)>,
}

impl VideoFile {
    /// The `tt` IMDb id, as Stremio add-ons take it.
    pub fn imdb_tt(&self) -> Option<String> {
        self.imdb_id.map(|n| format!("tt{n:07}"))
    }
}

/// `tt0113277` → 113277.
pub fn parse_imdb(s: Option<&str>) -> Option<u64> {
    let s = s?.trim();
    s.strip_prefix("tt")
        .unwrap_or(s)
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
}

fn stem(p: &str) -> Option<String> {
    Path::new(p)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

fn duration(media_info: Option<&str>) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(media_info?).ok()?;
    v.get("duration_secs")?.as_f64().filter(|d| *d > 0.0)
}

type Row = (
    String,         // folder
    String,         // relative path
    i64,            // size
    Option<String>, // scene name
    Option<String>, // original path
    Option<String>, // media info
    Option<String>, // imdb id
    i64,            // tmdb id
    i64,            // custom
    String,         // title
    Option<i64>,    // year
    Option<i64>,    // season
    Option<i64>,    // episode
);

pub async fn video_file(db: &Db, kind: SubtitleMediaKind, id: i64) -> Result<Option<VideoFile>> {
    let sql = match kind {
        SubtitleMediaKind::Movie => {
            "SELECT m.path, f.relative_path, f.size, f.scene_name, f.original_path, f.media_info,
                    m.imdb_id, m.tmdb_id, m.custom, m.title, m.year, NULL, NULL
             FROM movie_files f JOIN movies m ON m.id = f.movie_id WHERE f.id = ?"
        }
        SubtitleMediaKind::Episode => {
            "SELECT s.path, f.relative_path, f.size, f.scene_name, f.original_path, f.media_info,
                    s.imdb_id, s.tmdb_id, s.custom, s.title, s.year, e.season_number, e.episode_number
             FROM episode_files f JOIN series s ON s.id = f.series_id
             LEFT JOIN episodes e ON e.episode_file_id = f.id
             WHERE f.id = ? ORDER BY e.season_number, e.episode_number LIMIT 1"
        }
    };
    let row: Option<Row> = sqlx::query_as(sql).bind(id).fetch_optional(db.pool()).await?;
    Ok(row.map(
        |(folder, rel, size, scene, original, info, imdb, tmdb, custom, title, year, season, episode)| {
            let release = scene
                .filter(|s| !s.trim().is_empty())
                .or_else(|| original.as_deref().and_then(stem))
                .or_else(|| stem(&rel));
            VideoFile {
                kind,
                id,
                path: Path::new(&folder).join(&rel),
                size: size.max(0) as u64,
                release,
                duration: duration(info.as_deref()),
                imdb_id: parse_imdb(imdb.as_deref()),
                tmdb_id: (custom == 0 && tmdb > 0).then_some(tmdb as u64),
                title,
                year: year.map(|y| y as i32),
                episode: match (season, episode) {
                    (Some(s), Some(e)) if s >= 0 && e >= 0 => Some((s as u32, e as u32)),
                    _ => None,
                },
            }
        },
    ))
}

/// A subtitle to record.
#[derive(Debug, Clone)]
pub struct NewRecord<'a> {
    pub kind: SubtitleMediaKind,
    pub file_id: i64,
    pub language: &'a str,
    pub variant: Variant,
    pub path: &'a Path,
    pub source: &'a str,
    pub source_id: &'a str,
    pub level: MatchLevel,
    pub release: Option<&'a str>,
}

/// Records a fetched subtitle, replacing the row for the same file, language and variant.
pub async fn save(db: &Db, r: &NewRecord<'_>) -> Result<()> {
    sqlx::query(
        "INSERT INTO subtitle_files
            (media_kind, file_id, language, variant, path, source, source_id, match_level, release_name, fetched_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (media_kind, file_id, language, variant) DO UPDATE SET
            path = excluded.path, source = excluded.source, source_id = excluded.source_id,
            match_level = excluded.match_level, release_name = excluded.release_name,
            fetched_at = excluded.fetched_at",
    )
    .bind(r.kind.as_str())
    .bind(r.file_id)
    .bind(r.language)
    .bind(r.variant.as_str())
    .bind(r.path.to_string_lossy().into_owned())
    .bind(r.source)
    .bind(r.source_id)
    .bind(r.level.as_str())
    .bind(r.release)
    .bind(reelix_db::now_ms())
    .execute(db.pool())
    .await?;
    Ok(())
}

type RecordRow = (
    i64,
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
);

/// The subtitles recorded for a file.
pub async fn records(db: &Db, kind: SubtitleMediaKind, file_id: i64) -> Result<Vec<SubtitleFileDto>> {
    let rows: Vec<RecordRow> = sqlx::query_as(
        "SELECT id, media_kind, file_id, language, variant, path, source, match_level, release_name, fetched_at
         FROM subtitle_files WHERE media_kind = ? AND file_id = ? ORDER BY language, variant",
    )
    .bind(kind.as_str())
    .bind(file_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, _, file_id, language, variant, path, source, match_level, release_name, at)| {
                SubtitleFileDto {
                    id,
                    media_kind: kind,
                    file_id,
                    language,
                    variant,
                    path,
                    source,
                    match_level,
                    release_name,
                    fetched_at: chrono::Utc.timestamp_millis_opt(at).single().unwrap_or_default(),
                }
            },
        )
        .collect())
}

/// Notes a search and when the schedule may search the file again.
pub async fn mark_searched(db: &Db, kind: SubtitleMediaKind, file_id: i64, next_at: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO subtitle_searches (media_kind, file_id, last_searched_at, next_search_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (media_kind, file_id) DO UPDATE SET
            last_searched_at = excluded.last_searched_at, next_search_at = excluded.next_search_at",
    )
    .bind(kind.as_str())
    .bind(file_id)
    .bind(reelix_db::now_ms())
    .bind(next_at)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Puts a file at the front of the schedule (a new import).
pub async fn mark_due_now(db: &Db, kind: SubtitleMediaKind, file_id: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO subtitle_searches (media_kind, file_id, last_searched_at, next_search_at)
         VALUES (?, ?, 0, 0)
         ON CONFLICT (media_kind, file_id) DO UPDATE SET next_search_at = 0",
    )
    .bind(kind.as_str())
    .bind(file_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Forgets every search time, so the schedule looks at every file again (languages changed).
pub async fn clear_searches(db: &Db) -> Result<()> {
    sqlx::query("DELETE FROM subtitle_searches")
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Files due a search, new imports and files never searched first, then the longest waiting.
pub async fn due(db: &Db, now: i64, limit: u32) -> Result<Vec<(SubtitleMediaKind, i64)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT kind, id FROM (
            SELECT 'movie' AS kind, f.id AS id, COALESCE(s.next_search_at, -1) AS next
            FROM movie_files f
            LEFT JOIN subtitle_searches s ON s.media_kind = 'movie' AND s.file_id = f.id
            WHERE s.next_search_at IS NULL OR s.next_search_at <= ?1
            UNION ALL
            SELECT 'episode', f.id, COALESCE(s.next_search_at, -1)
            FROM episode_files f
            LEFT JOIN subtitle_searches s ON s.media_kind = 'episode' AND s.file_id = f.id
            WHERE s.next_search_at IS NULL OR s.next_search_at <= ?1
         ) ORDER BY CASE WHEN next = 0 THEN 0 ELSE 1 END, next, kind DESC, id LIMIT ?2",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(k, id)| SubtitleMediaKind::parse(&k).map(|k| (k, id)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imdb_ids() {
        assert_eq!(parse_imdb(Some("tt0113277")), Some(113277));
        assert_eq!(parse_imdb(Some("113277")), Some(113277));
        assert_eq!(parse_imdb(Some("")), None);
        assert_eq!(parse_imdb(None), None);
    }

    #[test]
    fn duration_from_media_details() {
        assert_eq!(
            duration(Some(r#"{"container":"matroska","duration_secs":6120.5}"#)),
            Some(6120.5)
        );
        assert_eq!(duration(Some("{}")), None);
        assert_eq!(duration(None), None);
    }
}
