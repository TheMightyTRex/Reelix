use chrono::{DateTime, Utc};
use reelix_core::Protocol;
use reelix_db::{Db, Result, from_ms, now_ms};
use serde::{Deserialize, Serialize};

use super::{ContentRef, GrabbedRelease, from_json, json, parse_protocol, protocol_str};

/// Lifecycle of a download Reelix is responsible for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackedState {
    /// In the client (queued, downloading, paused or post-processing).
    Downloading,
    /// Finished in the client; waiting for import (or for manual interaction).
    ImportPending,
    Importing,
    Imported,
    /// Failed in the client; failure handling done.
    Failed,
    /// Failed, but handling (blocklist / re-search) not done yet.
    FailedPending,
    /// User chose to ignore it.
    Ignored,
    /// Removed from the client by the user or after seeding goals.
    Removed,
}

impl TrackedState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::ImportPending => "import_pending",
            Self::Importing => "importing",
            Self::Imported => "imported",
            Self::Failed => "failed",
            Self::FailedPending => "failed_pending",
            Self::Ignored => "ignored",
            Self::Removed => "removed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "downloading" => Self::Downloading,
            "import_pending" => Self::ImportPending,
            "importing" => Self::Importing,
            "imported" => Self::Imported,
            "failed" => Self::Failed,
            "failed_pending" => Self::FailedPending,
            "ignored" => Self::Ignored,
            "removed" => Self::Removed,
            _ => return None,
        })
    }

    /// Still shown in the queue (not finished or dismissed).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Downloading | Self::ImportPending | Self::Importing | Self::FailedPending
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewTracked {
    pub download_client_id: i64,
    pub download_id: String,
    pub protocol: Protocol,
    pub title: String,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_ids: Vec<i64>,
    pub release: GrabbedRelease,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackedDownload {
    pub id: i64,
    pub download_client_id: i64,
    pub download_id: String,
    pub protocol: Protocol,
    pub title: String,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_ids: Vec<i64>,
    pub release: Option<GrabbedRelease>,
    /// Book / album the download is for (set with [`set_content`]).
    pub content: ContentRef,
    pub state: TrackedState,
    pub status_messages: Vec<String>,
    pub output_path: Option<String>,
    pub added: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    download_client_id: i64,
    download_id: String,
    protocol: String,
    title: String,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    episode_ids: String,
    release: String,
    state: String,
    status_messages: String,
    output_path: Option<String>,
    added: i64,
    updated: i64,
    book_id: Option<i64>,
    book_format: Option<String>,
    album_id: Option<i64>,
    radio_episode_id: Option<i64>,
}

impl From<Row> for TrackedDownload {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            download_client_id: r.download_client_id,
            download_id: r.download_id,
            protocol: parse_protocol(&r.protocol).unwrap_or(Protocol::Torrent),
            title: r.title,
            movie_id: r.movie_id,
            series_id: r.series_id,
            episode_ids: from_json(&r.episode_ids),
            release: serde_json::from_str(&r.release).ok(),
            content: ContentRef::from_columns(
                r.book_id,
                r.book_format.as_deref(),
                r.album_id,
                r.radio_episode_id,
            ),
            state: TrackedState::parse(&r.state).unwrap_or(TrackedState::Downloading),
            status_messages: from_json(&r.status_messages),
            output_path: r.output_path,
            added: from_ms(r.added),
            updated: from_ms(r.updated),
        }
    }
}

const COLS: &str = "id, download_client_id, download_id, protocol, title, movie_id, series_id, episode_ids, \
    release, state, status_messages, output_path, added, updated, book_id, book_format, album_id, \
    radio_episode_id";

/// Inserts a new tracked download (state `downloading`); an existing (client, download id) row is replaced.
pub async fn insert(db: &Db, t: &NewTracked) -> Result<i64> {
    let now = now_ms();
    let id = sqlx::query_scalar(
        "INSERT INTO tracked_downloads (download_client_id, download_id, protocol, title, movie_id, series_id, \
         episode_ids, release, state, added, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'downloading', ?, ?) \
         ON CONFLICT (download_client_id, download_id) DO UPDATE SET title = excluded.title, \
         movie_id = excluded.movie_id, series_id = excluded.series_id, episode_ids = excluded.episode_ids, \
         release = excluded.release, state = 'downloading', status_messages = '[]', updated = excluded.updated, \
         book_id = NULL, book_format = NULL, album_id = NULL, radio_episode_id = NULL \
         RETURNING id",
    )
    .bind(t.download_client_id)
    .bind(&t.download_id)
    .bind(protocol_str(t.protocol))
    .bind(&t.title)
    .bind(t.movie_id)
    .bind(t.series_id)
    .bind(json(&t.episode_ids))
    .bind(json(&t.release))
    .bind(now)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

/// Points a download at a book (format), album or radio episode.
pub async fn set_content(db: &Db, id: i64, c: &ContentRef) -> Result<()> {
    sqlx::query(
        "UPDATE tracked_downloads SET book_id = ?, book_format = ?, album_id = ?, radio_episode_id = ?, \
         updated = ? WHERE id = ?",
    )
    .bind(c.book_id)
    .bind(c.format_str())
    .bind(c.album_id)
    .bind(c.radio_episode_id)
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn get(db: &Db, id: i64) -> Result<Option<TrackedDownload>> {
    let row: Option<Row> = sqlx::query_as(&format!("SELECT {COLS} FROM tracked_downloads WHERE id = ?"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    Ok(row.map(Into::into))
}

pub async fn by_download(db: &Db, client_id: i64, download_id: &str) -> Result<Option<TrackedDownload>> {
    let row: Option<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM tracked_downloads WHERE download_client_id = ? AND download_id = ?"
    ))
    .bind(client_id)
    .bind(download_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(Into::into))
}

/// Downloads still in the queue (see [`TrackedState::is_active`]), oldest first.
pub async fn active(db: &Db) -> Result<Vec<TrackedDownload>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM tracked_downloads \
         WHERE state IN ('downloading','import_pending','importing','failed_pending') ORDER BY added, id"
    ))
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn set_state(db: &Db, id: i64, state: TrackedState, messages: &[String]) -> Result<()> {
    sqlx::query("UPDATE tracked_downloads SET state = ?, status_messages = ?, updated = ? WHERE id = ?")
        .bind(state.as_str())
        .bind(json(&messages))
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn set_output_path(db: &Db, id: i64, path: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE tracked_downloads SET output_path = ?, updated = ? WHERE id = ?")
        .bind(path)
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Re-points a download at different media (manual import / queue edit).
pub async fn set_media(
    db: &Db,
    id: i64,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    episode_ids: &[i64],
) -> Result<()> {
    sqlx::query(
        "UPDATE tracked_downloads SET movie_id = ?, series_id = ?, episode_ids = ?, updated = ? WHERE id = ?",
    )
    .bind(movie_id)
    .bind(series_id)
    .bind(json(&episode_ids))
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    Ok(sqlx::query("DELETE FROM tracked_downloads WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?
        .rows_affected()
        > 0)
}

/// Forgets finished rows older than the cutoff (housekeeping).
pub async fn prune_finished(db: &Db, older_than_ms: i64) -> Result<u64> {
    Ok(sqlx::query(
        "DELETE FROM tracked_downloads WHERE state IN ('imported','failed','ignored','removed') AND updated < ?",
    )
    .bind(older_than_ms)
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// Rows in one state, oldest first.
pub async fn with_state(db: &Db, state: TrackedState) -> Result<Vec<TrackedDownload>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM tracked_downloads WHERE state = ? ORDER BY added, id"
    ))
    .bind(state.as_str())
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// The newest row with this client download id (any client).
pub async fn by_download_id(db: &Db, download_id: &str) -> Result<Option<TrackedDownload>> {
    let row: Option<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM tracked_downloads WHERE download_id = ? ORDER BY added DESC, id DESC LIMIT 1"
    ))
    .bind(download_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(Into::into))
}
