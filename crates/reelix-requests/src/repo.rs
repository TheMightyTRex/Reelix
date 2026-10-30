//! SQL for requests, issues, comments, user administration and notification preferences.

use std::collections::HashMap;

use reelix_api_types::requests::{IssueStatus, IssueType, NotificationPrefsDto, RequestStatus, UserBriefDto};
use reelix_core::{MediaKind, Permissions};
use reelix_db::{Db, from_ms, now_ms};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::{Error, Result};

pub fn kind_str(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
    }
}

pub fn parse_kind(s: &str) -> Option<MediaKind> {
    match s {
        "movie" | "movies" => Some(MediaKind::Movie),
        "series" | "tv" | "show" => Some(MediaKind::Series),
        _ => None,
    }
}

fn decode(msg: impl Into<String>) -> Error {
    Error::Other(anyhow::anyhow!(msg.into()))
}

fn json_vec<T: serde::de::DeserializeOwned>(s: &str) -> Vec<T> {
    serde_json::from_str(s).unwrap_or_default()
}

// ------------------------------------------------------------------ requests

#[derive(Debug, Clone, PartialEq)]
pub struct RequestRow {
    pub id: i64,
    pub user_id: i64,
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub seasons: Vec<u32>,
    pub status: RequestStatus,
    pub is_4k: bool,
    pub profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
    pub modified_by: Option<i64>,
    pub decline_reason: Option<String>,
    pub failure_reason: Option<String>,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub approved_at: Option<i64>,
    pub available_at: Option<i64>,
}

/// Fields for a new request.
#[derive(Debug, Clone)]
pub struct NewRequest {
    pub user_id: i64,
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub seasons: Vec<u32>,
    pub is_4k: bool,
    pub profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
    pub source: &'static str,
}

fn map_request(r: &SqliteRow) -> Result<RequestRow> {
    let kind: String = r.try_get("media_kind")?;
    let status: String = r.try_get("status")?;
    Ok(RequestRow {
        id: r.try_get("id")?,
        user_id: r.try_get("user_id")?,
        kind: parse_kind(&kind).ok_or_else(|| decode(format!("bad media_kind {kind}")))?,
        tmdb_id: r.try_get::<i64, _>("tmdb_id")? as u32,
        tvdb_id: r.try_get::<Option<i64>, _>("tvdb_id")?.map(|v| v as u32),
        imdb_id: r.try_get("imdb_id")?,
        title: r.try_get("title")?,
        year: r.try_get::<Option<i64>, _>("year")?.map(|v| v as i32),
        poster: r.try_get("poster")?,
        backdrop: r.try_get("backdrop")?,
        seasons: json_vec(&r.try_get::<String, _>("seasons")?),
        status: RequestStatus::parse(&status).ok_or_else(|| decode(format!("bad status {status}")))?,
        is_4k: r.try_get::<i64, _>("is_4k")? != 0,
        profile_id: r.try_get("profile_id")?,
        root_folder_id: r.try_get("root_folder_id")?,
        tags: json_vec(&r.try_get::<String, _>("tags")?),
        modified_by: r.try_get("modified_by")?,
        decline_reason: r.try_get("decline_reason")?,
        failure_reason: r.try_get("failure_reason")?,
        movie_id: r.try_get("movie_id")?,
        series_id: r.try_get("series_id")?,
        source: r.try_get("source")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        approved_at: r.try_get("approved_at")?,
        available_at: r.try_get("available_at")?,
    })
}

/// Filters for listing requests.
#[derive(Debug, Clone, Default)]
pub struct RequestFilter {
    pub statuses: Vec<RequestStatus>,
    pub kind: Option<MediaKind>,
    pub user_id: Option<i64>,
    pub tmdb_ids: Vec<u32>,
    pub is_4k: Option<bool>,
}

impl RequestFilter {
    fn push(&self, q: &mut QueryBuilder<'_, Sqlite>) {
        // Book/music rows share the table (M8/M9) but are not `RequestRow`s.
        q.push(" WHERE media_kind IN ('movie', 'series')");
        if !self.statuses.is_empty() {
            q.push(" AND status IN (");
            let mut sep = q.separated(", ");
            for s in &self.statuses {
                sep.push_bind(s.as_str());
            }
            q.push(")");
        }
        if let Some(k) = self.kind {
            q.push(" AND media_kind = ").push_bind(kind_str(k));
        }
        if let Some(u) = self.user_id {
            q.push(" AND user_id = ").push_bind(u);
        }
        if let Some(b) = self.is_4k {
            q.push(" AND is_4k = ").push_bind(b as i64);
        }
        if !self.tmdb_ids.is_empty() {
            q.push(" AND tmdb_id IN (");
            let mut sep = q.separated(", ");
            for id in &self.tmdb_ids {
                sep.push_bind(*id as i64);
            }
            q.push(")");
        }
    }
}

pub struct RequestRepo<'a>(pub &'a Db);

impl RequestRepo<'_> {
    pub async fn create(&self, n: NewRequest) -> Result<i64> {
        let now = now_ms();
        let res = sqlx::query(
            "INSERT INTO requests (user_id, media_kind, tmdb_id, tvdb_id, imdb_id, title, year, poster, backdrop,
                seasons, status, is_4k, profile_id, root_folder_id, tags, source, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(n.user_id)
        .bind(kind_str(n.kind))
        .bind(n.tmdb_id as i64)
        .bind(n.tvdb_id.map(|v| v as i64))
        .bind(&n.imdb_id)
        .bind(&n.title)
        .bind(n.year.map(|v| v as i64))
        .bind(&n.poster)
        .bind(&n.backdrop)
        .bind(serde_json::to_string(&n.seasons).unwrap_or_else(|_| "[]".into()))
        .bind(n.is_4k as i64)
        .bind(n.profile_id)
        .bind(n.root_folder_id)
        .bind(serde_json::to_string(&n.tags).unwrap_or_else(|_| "[]".into()))
        .bind(n.source)
        .bind(now)
        .bind(now)
        .execute(self.0.pool())
        .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn get(&self, id: i64) -> Result<Option<RequestRow>> {
        let row = sqlx::query("SELECT * FROM requests WHERE id = ? AND media_kind IN ('movie', 'series')")
            .bind(id)
            .fetch_optional(self.0.pool())
            .await?;
        row.as_ref().map(map_request).transpose()
    }

    /// Every request matching `f`, newest first.
    pub async fn find(&self, f: &RequestFilter) -> Result<Vec<RequestRow>> {
        let mut q = QueryBuilder::<Sqlite>::new("SELECT * FROM requests");
        f.push(&mut q);
        q.push(" ORDER BY created_at DESC, id DESC");
        let rows = q.build().fetch_all(self.0.pool()).await?;
        rows.iter().map(map_request).collect()
    }

    /// One page plus the total count. `sort`: created | updated | title.
    pub async fn page(
        &self,
        f: &RequestFilter,
        sort: &str,
        asc: bool,
        limit: u32,
        offset: u64,
    ) -> Result<(Vec<RequestRow>, u64)> {
        let mut c = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM requests");
        f.push(&mut c);
        let total: i64 = c.build_query_scalar().fetch_one(self.0.pool()).await?;
        let col = match sort {
            "updated" => "updated_at",
            "title" => "title COLLATE NOCASE",
            _ => "created_at",
        };
        let dir = if asc { "ASC" } else { "DESC" };
        let mut q = QueryBuilder::<Sqlite>::new("SELECT * FROM requests");
        f.push(&mut q);
        q.push(format!(" ORDER BY {col} {dir}, id {dir} LIMIT "))
            .push_bind(limit as i64)
            .push(" OFFSET ")
            .push_bind(offset as i64);
        let rows = q.build().fetch_all(self.0.pool()).await?;
        Ok((rows.iter().map(map_request).collect::<Result<_>>()?, total as u64))
    }

    /// Counts per status and kind for `f` (status/kind filters of `f` are ignored).
    pub async fn counts(&self, user_id: Option<i64>) -> Result<Vec<(String, String, i64)>> {
        let mut q = QueryBuilder::<Sqlite>::new("SELECT status, media_kind, COUNT(*) AS n FROM requests");
        RequestFilter {
            user_id,
            ..Default::default()
        }
        .push(&mut q);
        q.push(" GROUP BY status, media_kind");
        let rows = q.build().fetch_all(self.0.pool()).await?;
        rows.iter()
            .map(|r| Ok((r.try_get("status")?, r.try_get("media_kind")?, r.try_get("n")?)))
            .collect()
    }

    /// Persists every mutable field of `r` (updated_at = now).
    pub async fn save(&self, r: &RequestRow) -> Result<()> {
        sqlx::query(
            "UPDATE requests SET seasons = ?, status = ?, is_4k = ?, profile_id = ?, root_folder_id = ?, tags = ?,
                modified_by = ?, decline_reason = ?, failure_reason = ?, movie_id = ?, series_id = ?,
                approved_at = ?, available_at = ?, updated_at = ?
             WHERE id = ? AND media_kind IN ('movie', 'series')",
        )
        .bind(serde_json::to_string(&r.seasons).unwrap_or_else(|_| "[]".into()))
        .bind(r.status.as_str())
        .bind(r.is_4k as i64)
        .bind(r.profile_id)
        .bind(r.root_folder_id)
        .bind(serde_json::to_string(&r.tags).unwrap_or_else(|_| "[]".into()))
        .bind(r.modified_by)
        .bind(&r.decline_reason)
        .bind(&r.failure_reason)
        .bind(r.movie_id)
        .bind(r.series_id)
        .bind(r.approved_at)
        .bind(r.available_at)
        .bind(now_ms())
        .bind(r.id)
        .execute(self.0.pool())
        .await?;
        Ok(())
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        let r = sqlx::query("DELETE FROM requests WHERE id = ? AND media_kind IN ('movie', 'series')")
            .bind(id)
            .execute(self.0.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Non-declined requests of `user_id` and `kind` created at or after `since_ms`.
    pub async fn since(&self, user_id: i64, kind: MediaKind, since_ms: i64) -> Result<Vec<RequestRow>> {
        let rows = sqlx::query(
            "SELECT * FROM requests WHERE user_id = ? AND media_kind = ? AND created_at >= ? AND status != 'declined'",
        )
        .bind(user_id)
        .bind(kind_str(kind))
        .bind(since_ms)
        .fetch_all(self.0.pool())
        .await?;
        rows.iter().map(map_request).collect()
    }

    /// Every request of the user, of any kind (movie, series, book, music).
    pub async fn count_for_user(&self, user_id: i64) -> Result<u64> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE user_id = ?")
            .bind(user_id)
            .fetch_one(self.0.pool())
            .await?;
        Ok(n as u64)
    }
}

// ------------------------------------------------------------------ issues

#[derive(Debug, Clone, PartialEq)]
pub struct IssueRow {
    pub id: i64,
    pub user_id: i64,
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub issue_type: IssueType,
    pub status: IssueStatus,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub message: String,
    pub resolved_by: Option<i64>,
    pub comment_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommentRow {
    pub id: i64,
    pub issue_id: i64,
    pub user_id: i64,
    pub message: String,
    pub created_at: i64,
}

const ISSUE_SELECT: &str = "SELECT i.*, (SELECT COUNT(*) FROM issue_comments c WHERE c.issue_id = i.id) AS comment_count FROM issues i";

fn map_issue(r: &SqliteRow) -> Result<IssueRow> {
    let kind: String = r.try_get("media_kind")?;
    let t: String = r.try_get("issue_type")?;
    let s: String = r.try_get("status")?;
    Ok(IssueRow {
        id: r.try_get("id")?,
        user_id: r.try_get("user_id")?,
        kind: parse_kind(&kind).ok_or_else(|| decode(format!("bad media_kind {kind}")))?,
        tmdb_id: r.try_get::<i64, _>("tmdb_id")? as u32,
        title: r.try_get("title")?,
        year: r.try_get::<Option<i64>, _>("year")?.map(|v| v as i32),
        poster: r.try_get("poster")?,
        issue_type: IssueType::parse(&t).ok_or_else(|| decode(format!("bad issue_type {t}")))?,
        status: IssueStatus::parse(&s).ok_or_else(|| decode(format!("bad status {s}")))?,
        season: r.try_get::<Option<i64>, _>("season")?.map(|v| v as u32),
        episode: r.try_get::<Option<i64>, _>("episode")?.map(|v| v as u32),
        message: r.try_get("message")?,
        resolved_by: r.try_get("resolved_by")?,
        comment_count: r.try_get::<i64, _>("comment_count")? as u32,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        resolved_at: r.try_get("resolved_at")?,
    })
}

#[derive(Debug, Clone, Default)]
pub struct IssueFilter {
    pub status: Option<IssueStatus>,
    pub issue_type: Option<IssueType>,
    pub kind: Option<MediaKind>,
    pub tmdb_id: Option<u32>,
    pub user_id: Option<i64>,
}

impl IssueFilter {
    fn push(&self, q: &mut QueryBuilder<'_, Sqlite>) {
        // Book/music rows share the table (M8/M9) but are not `RequestRow`s.
        q.push(" WHERE media_kind IN ('movie', 'series')");
        if let Some(s) = self.status {
            q.push(" AND i.status = ").push_bind(s.as_str());
        }
        if let Some(t) = self.issue_type {
            q.push(" AND i.issue_type = ").push_bind(t.as_str());
        }
        if let Some(k) = self.kind {
            q.push(" AND i.media_kind = ").push_bind(kind_str(k));
        }
        if let Some(t) = self.tmdb_id {
            q.push(" AND i.tmdb_id = ").push_bind(t as i64);
        }
        if let Some(u) = self.user_id {
            q.push(" AND i.user_id = ").push_bind(u);
        }
    }
}

pub struct NewIssue {
    pub user_id: i64,
    pub kind: MediaKind,
    pub tmdb_id: u32,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub issue_type: IssueType,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub message: String,
}

pub struct IssueRepo<'a>(pub &'a Db);

impl IssueRepo<'_> {
    pub async fn create(&self, n: NewIssue) -> Result<i64> {
        let now = now_ms();
        let r = sqlx::query(
            "INSERT INTO issues (user_id, media_kind, tmdb_id, title, year, poster, issue_type, status, season, episode,
                message, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'open', ?, ?, ?, ?, ?)",
        )
        .bind(n.user_id)
        .bind(kind_str(n.kind))
        .bind(n.tmdb_id as i64)
        .bind(&n.title)
        .bind(n.year.map(|v| v as i64))
        .bind(&n.poster)
        .bind(n.issue_type.as_str())
        .bind(n.season.map(|v| v as i64))
        .bind(n.episode.map(|v| v as i64))
        .bind(&n.message)
        .bind(now)
        .bind(now)
        .execute(self.0.pool())
        .await?;
        Ok(r.last_insert_rowid())
    }

    pub async fn get(&self, id: i64) -> Result<Option<IssueRow>> {
        let row = sqlx::query(&format!("{ISSUE_SELECT} WHERE i.id = ?"))
            .bind(id)
            .fetch_optional(self.0.pool())
            .await?;
        row.as_ref().map(map_issue).transpose()
    }

    pub async fn page(&self, f: &IssueFilter, limit: u32, offset: u64) -> Result<(Vec<IssueRow>, u64)> {
        let mut c = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM issues i");
        f.push(&mut c);
        let total: i64 = c.build_query_scalar().fetch_one(self.0.pool()).await?;
        let mut q = QueryBuilder::<Sqlite>::new(ISSUE_SELECT);
        f.push(&mut q);
        q.push(" ORDER BY i.status = 'resolved', i.updated_at DESC, i.id DESC LIMIT ")
            .push_bind(limit as i64)
            .push(" OFFSET ")
            .push_bind(offset as i64);
        let rows = q.build().fetch_all(self.0.pool()).await?;
        Ok((rows.iter().map(map_issue).collect::<Result<_>>()?, total as u64))
    }

    pub async fn counts(&self, user_id: Option<i64>) -> Result<Vec<(String, String, i64)>> {
        let mut q = QueryBuilder::<Sqlite>::new("SELECT i.status, i.issue_type, COUNT(*) AS n FROM issues i");
        IssueFilter {
            user_id,
            ..Default::default()
        }
        .push(&mut q);
        q.push(" GROUP BY i.status, i.issue_type");
        let rows = q.build().fetch_all(self.0.pool()).await?;
        rows.iter()
            .map(|r| Ok((r.try_get("status")?, r.try_get("issue_type")?, r.try_get("n")?)))
            .collect()
    }

    pub async fn save(&self, i: &IssueRow) -> Result<()> {
        sqlx::query(
            "UPDATE issues SET issue_type = ?, status = ?, season = ?, episode = ?, message = ?, resolved_by = ?,
                resolved_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(i.issue_type.as_str())
        .bind(i.status.as_str())
        .bind(i.season.map(|v| v as i64))
        .bind(i.episode.map(|v| v as i64))
        .bind(&i.message)
        .bind(i.resolved_by)
        .bind(i.resolved_at)
        .bind(now_ms())
        .bind(i.id)
        .execute(self.0.pool())
        .await?;
        Ok(())
    }

    pub async fn touch(&self, id: i64) -> Result<()> {
        sqlx::query("UPDATE issues SET updated_at = ? WHERE id = ?")
            .bind(now_ms())
            .bind(id)
            .execute(self.0.pool())
            .await?;
        Ok(())
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        let r = sqlx::query("DELETE FROM issues WHERE id = ?")
            .bind(id)
            .execute(self.0.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn add_comment(&self, issue_id: i64, user_id: i64, message: &str) -> Result<i64> {
        let r = sqlx::query(
            "INSERT INTO issue_comments (issue_id, user_id, message, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(issue_id)
        .bind(user_id)
        .bind(message)
        .bind(now_ms())
        .execute(self.0.pool())
        .await?;
        Ok(r.last_insert_rowid())
    }

    pub async fn comments(&self, issue_id: i64) -> Result<Vec<CommentRow>> {
        let rows = sqlx::query("SELECT * FROM issue_comments WHERE issue_id = ? ORDER BY id")
            .bind(issue_id)
            .fetch_all(self.0.pool())
            .await?;
        rows.iter()
            .map(|r| {
                Ok(CommentRow {
                    id: r.try_get("id")?,
                    issue_id: r.try_get("issue_id")?,
                    user_id: r.try_get("user_id")?,
                    message: r.try_get("message")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }
}

// ------------------------------------------------------------------ users

/// A full user row, including the columns M6 added.
#[derive(Debug, Clone, PartialEq)]
pub struct UserRecord {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub avatar: Option<String>,
    pub auth_source: String,
    pub external_id: Option<String>,
    pub permissions: Permissions,
    pub locale: Option<String>,
    pub movie_quota_limit: Option<u32>,
    pub movie_quota_days: Option<u32>,
    pub tv_quota_limit: Option<u32>,
    pub tv_quota_days: Option<u32>,
    pub book_quota_limit: Option<u32>,
    pub book_quota_days: Option<u32>,
    pub music_quota_limit: Option<u32>,
    pub music_quota_days: Option<u32>,
    pub has_password: bool,
    /// Encrypted plex.tv token.
    pub plex_token: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl UserRecord {
    pub fn display(&self) -> String {
        self.display_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.username.clone())
    }

    pub fn brief(&self) -> UserBriefDto {
        UserBriefDto {
            id: self.id,
            username: self.username.clone(),
            display_name: self.display(),
            avatar: self.avatar.clone(),
        }
    }
}

fn opt_u32(r: &SqliteRow, col: &str) -> Result<Option<u32>> {
    Ok(r.try_get::<Option<i64>, _>(col)?.map(|v| v.max(0) as u32))
}

fn map_user(r: &SqliteRow) -> Result<UserRecord> {
    Ok(UserRecord {
        id: r.try_get("id")?,
        username: r.try_get("username")?,
        email: r.try_get("email")?,
        display_name: r.try_get("display_name")?,
        avatar: r.try_get("avatar")?,
        auth_source: r.try_get("auth_source")?,
        external_id: r.try_get("external_id")?,
        permissions: Permissions::from_bits_truncate(r.try_get::<i64, _>("permissions")? as u64),
        locale: r.try_get("locale")?,
        movie_quota_limit: opt_u32(r, "movie_quota_limit")?,
        movie_quota_days: opt_u32(r, "movie_quota_days")?,
        tv_quota_limit: opt_u32(r, "tv_quota_limit")?,
        tv_quota_days: opt_u32(r, "tv_quota_days")?,
        book_quota_limit: opt_u32(r, "book_quota_limit")?,
        book_quota_days: opt_u32(r, "book_quota_days")?,
        music_quota_limit: opt_u32(r, "music_quota_limit")?,
        music_quota_days: opt_u32(r, "music_quota_days")?,
        has_password: r.try_get::<Option<String>, _>("password_hash")?.is_some(),
        plex_token: r.try_get("plex_token")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

/// Editable user fields (`None` password = unchanged).
#[derive(Debug, Clone)]
pub struct UserEdit {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub avatar: Option<String>,
    pub locale: Option<String>,
    pub permissions: Permissions,
    pub movie_quota_limit: Option<u32>,
    pub movie_quota_days: Option<u32>,
    pub tv_quota_limit: Option<u32>,
    pub tv_quota_days: Option<u32>,
    pub book_quota_limit: Option<u32>,
    pub book_quota_days: Option<u32>,
    pub music_quota_limit: Option<u32>,
    pub music_quota_days: Option<u32>,
}

pub struct UserAdminRepo<'a>(pub &'a Db);

impl UserAdminRepo<'_> {
    pub async fn get(&self, id: i64) -> Result<Option<UserRecord>> {
        let row = sqlx::query("SELECT * FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(self.0.pool())
            .await?;
        row.as_ref().map(map_user).transpose()
    }

    pub async fn list(&self) -> Result<Vec<UserRecord>> {
        let rows = sqlx::query("SELECT * FROM users ORDER BY id")
            .fetch_all(self.0.pool())
            .await?;
        rows.iter().map(map_user).collect()
    }

    pub async fn by_external(&self, source: &str, external_id: &str) -> Result<Option<UserRecord>> {
        let row = sqlx::query("SELECT * FROM users WHERE auth_source = ? AND external_id = ?")
            .bind(source)
            .bind(external_id)
            .fetch_optional(self.0.pool())
            .await?;
        row.as_ref().map(map_user).transpose()
    }

    pub async fn username_taken(&self, username: &str) -> Result<bool> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ?")
            .bind(username)
            .fetch_one(self.0.pool())
            .await?;
        Ok(n > 0)
    }

    /// Links an external identity to a user and refreshes profile fields.
    pub async fn link_external(
        &self,
        id: i64,
        external_id: &str,
        email: Option<&str>,
        avatar: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE users SET external_id = ?, email = COALESCE(email, ?), avatar = COALESCE(?, avatar), updated_at = ?
             WHERE id = ?",
        )
        .bind(external_id)
        .bind(email)
        .bind(avatar)
        .bind(now_ms())
        .bind(id)
        .execute(self.0.pool())
        .await?;
        Ok(())
    }

    pub async fn update(&self, id: i64, e: &UserEdit) -> Result<()> {
        sqlx::query(
            "UPDATE users SET display_name = ?, email = ?, avatar = ?, locale = ?, permissions = ?,
                movie_quota_limit = ?, movie_quota_days = ?, tv_quota_limit = ?, tv_quota_days = ?,
                book_quota_limit = ?, book_quota_days = ?, music_quota_limit = ?, music_quota_days = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(&e.display_name)
        .bind(&e.email)
        .bind(&e.avatar)
        .bind(&e.locale)
        .bind(e.permissions.bits() as i64)
        .bind(e.movie_quota_limit.map(i64::from))
        .bind(e.movie_quota_days.map(i64::from))
        .bind(e.tv_quota_limit.map(i64::from))
        .bind(e.tv_quota_days.map(i64::from))
        .bind(e.book_quota_limit.map(i64::from))
        .bind(e.book_quota_days.map(i64::from))
        .bind(e.music_quota_limit.map(i64::from))
        .bind(e.music_quota_days.map(i64::from))
        .bind(now_ms())
        .bind(id)
        .execute(self.0.pool())
        .await?;
        Ok(())
    }

    pub async fn set_password_hash(&self, id: i64, hash: &str) -> Result<()> {
        sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
            .bind(hash)
            .bind(now_ms())
            .bind(id)
            .execute(self.0.pool())
            .await?;
        Ok(())
    }

    pub async fn set_plex_token(&self, id: i64, encrypted: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE users SET plex_token = ?, updated_at = ? WHERE id = ?")
            .bind(encrypted)
            .bind(now_ms())
            .bind(id)
            .execute(self.0.pool())
            .await?;
        Ok(())
    }

    /// Ends every session of the user.
    pub async fn delete_sessions(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(id)
            .execute(self.0.pool())
            .await?;
        Ok(())
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        let r = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(self.0.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn count_admins(&self) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE (permissions & 1) = 1")
                .fetch_one(self.0.pool())
                .await?,
        )
    }

    /// Users whose permissions grant `p` (admins included).
    pub async fn with_permission(&self, p: Permissions) -> Result<Vec<i64>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|u| u.permissions.has(p))
            .map(|u| u.id)
            .collect())
    }

    pub async fn briefs(&self, ids: impl IntoIterator<Item = i64>) -> Result<HashMap<i64, UserBriefDto>> {
        let mut ids: Vec<i64> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut q = QueryBuilder::<Sqlite>::new("SELECT * FROM users WHERE id IN (");
        let mut sep = q.separated(", ");
        for id in &ids {
            sep.push_bind(*id);
        }
        q.push(")");
        let rows = q.build().fetch_all(self.0.pool()).await?;
        rows.iter()
            .map(|r| map_user(r).map(|u| (u.id, u.brief())))
            .collect()
    }

    pub async fn prefs(&self, id: i64) -> Result<NotificationPrefsDto> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT settings FROM user_notification_settings WHERE user_id = ?")
                .bind(id)
                .fetch_optional(self.0.pool())
                .await?;
        Ok(raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    pub async fn set_prefs(&self, id: i64, p: &NotificationPrefsDto) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_notification_settings (user_id, settings, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(user_id) DO UPDATE SET settings = excluded.settings, updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(serde_json::to_string(p).map_err(reelix_db::DbError::from)?)
        .bind(now_ms())
        .execute(self.0.pool())
        .await?;
        Ok(())
    }

    /// Users with a stored plex token and watchlist sync enabled: `(user, prefs)`.
    pub async fn watchlist_users(&self) -> Result<Vec<(UserRecord, NotificationPrefsDto)>> {
        let mut out = Vec::new();
        for u in self.list().await? {
            if u.plex_token.is_none() {
                continue;
            }
            let p = self.prefs(u.id).await?;
            if p.watchlist_sync_movies || p.watchlist_sync_series {
                out.push((u, p));
            }
        }
        Ok(out)
    }
}

pub fn ts(ms: i64) -> chrono::DateTime<chrono::Utc> {
    from_ms(ms)
}
