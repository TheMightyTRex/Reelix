use chrono::{DateTime, Utc};
use reelix_core::Protocol;
use reelix_db::{Db, Result, from_ms, now_ms};
use reelix_parser::{Language, QualityModel};
use serde::{Deserialize, Serialize};

use super::{FormatRef, from_json, json, parse_protocol, protocol_str};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEvent {
    Grabbed,
    Imported,
    DownloadFailed,
    ImportFailed,
    FileDeleted,
    Renamed,
    Ignored,
}

impl HistoryEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grabbed => "grabbed",
            Self::Imported => "imported",
            Self::DownloadFailed => "download_failed",
            Self::ImportFailed => "import_failed",
            Self::FileDeleted => "file_deleted",
            Self::Renamed => "renamed",
            Self::Ignored => "ignored",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "grabbed" => Self::Grabbed,
            "imported" => Self::Imported,
            "download_failed" => Self::DownloadFailed,
            "import_failed" => Self::ImportFailed,
            "file_deleted" => Self::FileDeleted,
            "renamed" => Self::Renamed,
            "ignored" => Self::Ignored,
            _ => return None,
        })
    }
}

/// A history row to insert. One row per episode for multi-episode releases.
#[derive(Debug, Clone, PartialEq)]
pub struct NewHistory {
    pub event: HistoryEvent,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_id: Option<i64>,
    pub source_title: String,
    pub quality: Option<QualityModel>,
    pub languages: Vec<Language>,
    pub custom_formats: Vec<FormatRef>,
    pub format_score: i32,
    pub download_id: Option<String>,
    pub protocol: Option<Protocol>,
    pub indexer: Option<String>,
    pub download_client: Option<String>,
    /// Event-specific details (paths, messages, release info…).
    pub data: serde_json::Value,
}

impl NewHistory {
    pub fn new(event: HistoryEvent, source_title: impl Into<String>) -> Self {
        Self {
            event,
            movie_id: None,
            series_id: None,
            episode_id: None,
            source_title: source_title.into(),
            quality: None,
            languages: Vec::new(),
            custom_formats: Vec::new(),
            format_score: 0,
            download_id: None,
            protocol: None,
            indexer: None,
            download_client: None,
            data: serde_json::json!({}),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub event: HistoryEvent,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_id: Option<i64>,
    pub source_title: String,
    pub quality: Option<QualityModel>,
    pub languages: Vec<Language>,
    pub custom_formats: Vec<FormatRef>,
    pub format_score: i32,
    pub download_id: Option<String>,
    pub protocol: Option<Protocol>,
    pub indexer: Option<String>,
    pub download_client: Option<String>,
    pub data: serde_json::Value,
    pub date: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    event_type: String,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    episode_id: Option<i64>,
    source_title: String,
    quality: Option<String>,
    languages: String,
    custom_formats: String,
    format_score: i64,
    download_id: Option<String>,
    protocol: Option<String>,
    indexer: Option<String>,
    download_client: Option<String>,
    data: String,
    date: i64,
}

impl From<Row> for HistoryEntry {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            event: HistoryEvent::parse(&r.event_type).unwrap_or(HistoryEvent::Ignored),
            movie_id: r.movie_id,
            series_id: r.series_id,
            episode_id: r.episode_id,
            source_title: r.source_title,
            quality: r.quality.and_then(|q| serde_json::from_str(&q).ok()),
            languages: from_json(&r.languages),
            custom_formats: from_json(&r.custom_formats),
            format_score: r.format_score as i32,
            download_id: r.download_id,
            protocol: r.protocol.as_deref().and_then(parse_protocol),
            indexer: r.indexer,
            download_client: r.download_client,
            data: serde_json::from_str(&r.data).unwrap_or_default(),
            date: from_ms(r.date),
        }
    }
}

/// Filters for [`list`]; `None` means "any".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HistoryFilter {
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_id: Option<i64>,
    pub event: Option<HistoryEvent>,
    pub download_id: Option<String>,
}

const COLS: &str = "id, event_type, movie_id, series_id, episode_id, source_title, quality, languages, \
    custom_formats, format_score, download_id, protocol, indexer, download_client, data, date";

pub async fn insert(db: &Db, h: &NewHistory) -> Result<i64> {
    let id = sqlx::query_scalar(
        "INSERT INTO history (event_type, movie_id, series_id, episode_id, source_title, quality, languages, \
         custom_formats, format_score, download_id, protocol, indexer, download_client, data, date) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(h.event.as_str())
    .bind(h.movie_id)
    .bind(h.series_id)
    .bind(h.episode_id)
    .bind(&h.source_title)
    .bind(h.quality.as_ref().map(json))
    .bind(json(&h.languages))
    .bind(json(&h.custom_formats))
    .bind(h.format_score as i64)
    .bind(&h.download_id)
    .bind(h.protocol.map(protocol_str))
    .bind(&h.indexer)
    .bind(&h.download_client)
    .bind(h.data.to_string())
    .bind(now_ms())
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

/// Newest first, with the total count for paging.
pub async fn list(db: &Db, f: &HistoryFilter, offset: i64, limit: i64) -> Result<(Vec<HistoryEntry>, i64)> {
    let mut wh = String::from(" WHERE 1=1");
    if f.movie_id.is_some() {
        wh.push_str(" AND movie_id = ?");
    }
    if f.series_id.is_some() {
        wh.push_str(" AND series_id = ?");
    }
    if f.episode_id.is_some() {
        wh.push_str(" AND episode_id = ?");
    }
    if f.event.is_some() {
        wh.push_str(" AND event_type = ?");
    }
    if f.download_id.is_some() {
        wh.push_str(" AND download_id = ?");
    }
    macro_rules! bind_all {
        ($q:expr) => {{
            let mut q = $q;
            if let Some(v) = f.movie_id {
                q = q.bind(v);
            }
            if let Some(v) = f.series_id {
                q = q.bind(v);
            }
            if let Some(v) = f.episode_id {
                q = q.bind(v);
            }
            if let Some(v) = f.event {
                q = q.bind(v.as_str());
            }
            if let Some(v) = &f.download_id {
                q = q.bind(v.clone());
            }
            q
        }};
    }
    let count_sql = format!("SELECT COUNT(*) FROM history{wh}");
    let total: i64 = bind_all!(sqlx::query_scalar(&count_sql))
        .fetch_one(db.pool())
        .await?;
    let sql = format!("SELECT {COLS} FROM history{wh} ORDER BY date DESC, id DESC LIMIT ? OFFSET ?");
    let rows: Vec<Row> = bind_all!(sqlx::query_as(&sql))
        .bind(limit)
        .bind(offset)
        .fetch_all(db.pool())
        .await?;
    Ok((rows.into_iter().map(Into::into).collect(), total))
}

/// The grab history rows for a download, oldest first.
pub async fn grabs_for_download(db: &Db, download_id: &str) -> Result<Vec<HistoryEntry>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM history WHERE download_id = ? AND event_type = 'grabbed' ORDER BY id"
    ))
    .bind(download_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn prune(db: &Db, older_than_ms: i64) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM history WHERE date < ?")
        .bind(older_than_ms)
        .execute(db.pool())
        .await?
        .rows_affected())
}

pub async fn get(db: &Db, id: i64) -> Result<Option<HistoryEntry>> {
    let row: Option<Row> = sqlx::query_as(&format!("SELECT {COLS} FROM history WHERE id = ?"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    Ok(row.map(Into::into))
}
