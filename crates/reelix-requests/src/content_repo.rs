//! SQL for book / album requests (`requests` rows with `media_kind` `book` or `music`).

use reelix_api_types::requests::{ContentRequestKind, RequestStatus};
use reelix_core::BookFormat;
use reelix_db::{Db, now_ms};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::{Error, Result};

fn decode(msg: impl Into<String>) -> Error {
    Error::Other(anyhow::anyhow!(msg.into()))
}

fn json_vec<T: serde::de::DeserializeOwned>(s: &str) -> Vec<T> {
    serde_json::from_str(s).unwrap_or_default()
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContentRow {
    pub id: i64,
    pub user_id: i64,
    pub kind: ContentRequestKind,
    pub foreign_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub formats: Vec<BookFormat>,
    pub status: RequestStatus,
    pub profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
    pub modified_by: Option<i64>,
    pub decline_reason: Option<String>,
    pub failure_reason: Option<String>,
    pub book_id: Option<i64>,
    pub author_id: Option<i64>,
    pub album_id: Option<i64>,
    pub artist_id: Option<i64>,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub approved_at: Option<i64>,
    pub available_at: Option<i64>,
}

impl ContentRow {
    /// Library id of the requested item (book / album).
    pub fn library_id(&self) -> Option<i64> {
        match self.kind {
            ContentRequestKind::Book => self.book_id,
            ContentRequestKind::Music => self.album_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewContent {
    pub user_id: i64,
    pub kind: ContentRequestKind,
    pub foreign_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub formats: Vec<BookFormat>,
    pub profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
    pub source: &'static str,
}

fn map(r: &SqliteRow) -> Result<ContentRow> {
    let kind: String = r.try_get("media_kind")?;
    let status: String = r.try_get("status")?;
    Ok(ContentRow {
        id: r.try_get("id")?,
        user_id: r.try_get("user_id")?,
        kind: ContentRequestKind::parse(&kind).ok_or_else(|| decode(format!("bad media_kind {kind}")))?,
        foreign_id: r.try_get::<Option<String>, _>("foreign_id")?.unwrap_or_default(),
        title: r.try_get("title")?,
        subtitle: r.try_get("subtitle")?,
        year: r.try_get::<Option<i64>, _>("year")?.map(|v| v as i32),
        poster: r.try_get("poster")?,
        formats: json_vec(&r.try_get::<String, _>("book_formats")?),
        status: RequestStatus::parse(&status).ok_or_else(|| decode(format!("bad status {status}")))?,
        profile_id: r.try_get("profile_id")?,
        root_folder_id: r.try_get("root_folder_id")?,
        tags: json_vec(&r.try_get::<String, _>("tags")?),
        modified_by: r.try_get("modified_by")?,
        decline_reason: r.try_get("decline_reason")?,
        failure_reason: r.try_get("failure_reason")?,
        book_id: r.try_get("book_id")?,
        author_id: r.try_get("author_id")?,
        album_id: r.try_get("album_id")?,
        artist_id: r.try_get("artist_id")?,
        source: r.try_get("source")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        approved_at: r.try_get("approved_at")?,
        available_at: r.try_get("available_at")?,
    })
}

/// Filters for listing book/album requests.
#[derive(Debug, Clone, Default)]
pub struct ContentFilter {
    pub statuses: Vec<RequestStatus>,
    pub kind: Option<ContentRequestKind>,
    pub user_id: Option<i64>,
    pub foreign_ids: Vec<String>,
}

impl ContentFilter {
    fn push(&self, q: &mut QueryBuilder<'_, Sqlite>) {
        q.push(" WHERE media_kind IN ('book', 'music')");
        if !self.statuses.is_empty() {
            q.push(" AND status IN (");
            let mut sep = q.separated(", ");
            for s in &self.statuses {
                sep.push_bind(s.as_str());
            }
            q.push(")");
        }
        if let Some(k) = self.kind {
            q.push(" AND media_kind = ").push_bind(k.as_str());
        }
        if let Some(u) = self.user_id {
            q.push(" AND user_id = ").push_bind(u);
        }
        if !self.foreign_ids.is_empty() {
            q.push(" AND foreign_id IN (");
            let mut sep = q.separated(", ");
            for id in &self.foreign_ids {
                sep.push_bind(id.clone());
            }
            q.push(")");
        }
    }
}

pub struct ContentRepo<'a>(pub &'a Db);

impl ContentRepo<'_> {
    pub async fn create(&self, n: NewContent) -> Result<i64> {
        let now = now_ms();
        let res = sqlx::query(
            "INSERT INTO requests (user_id, media_kind, foreign_id, title, subtitle, year, poster, book_formats,
                status, profile_id, root_folder_id, tags, source, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?)",
        )
        .bind(n.user_id)
        .bind(n.kind.as_str())
        .bind(&n.foreign_id)
        .bind(&n.title)
        .bind(&n.subtitle)
        .bind(n.year.map(i64::from))
        .bind(&n.poster)
        .bind(json(&n.formats))
        .bind(n.profile_id)
        .bind(n.root_folder_id)
        .bind(json(&n.tags))
        .bind(n.source)
        .bind(now)
        .bind(now)
        .execute(self.0.pool())
        .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn get(&self, id: i64) -> Result<Option<ContentRow>> {
        let row = sqlx::query("SELECT * FROM requests WHERE id = ? AND media_kind IN ('book', 'music')")
            .bind(id)
            .fetch_optional(self.0.pool())
            .await?;
        row.as_ref().map(map).transpose()
    }

    /// Every request matching `f`, newest first.
    pub async fn find(&self, f: &ContentFilter) -> Result<Vec<ContentRow>> {
        let mut q = QueryBuilder::<Sqlite>::new("SELECT * FROM requests");
        f.push(&mut q);
        q.push(" ORDER BY created_at DESC, id DESC");
        let rows = q.build().fetch_all(self.0.pool()).await?;
        rows.iter().map(map).collect()
    }

    /// One page plus the total count. `sort`: created | updated | title.
    pub async fn page(
        &self,
        f: &ContentFilter,
        sort: &str,
        asc: bool,
        limit: u32,
        offset: u64,
    ) -> Result<(Vec<ContentRow>, u64)> {
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
            .push_bind(i64::from(limit))
            .push(" OFFSET ")
            .push_bind(offset as i64);
        let rows = q.build().fetch_all(self.0.pool()).await?;
        Ok((rows.iter().map(map).collect::<Result<_>>()?, total as u64))
    }

    /// `(status, media_kind, n)` of book/music requests, optionally of one user.
    pub async fn counts(&self, user_id: Option<i64>) -> Result<Vec<(String, String, i64)>> {
        let mut q = QueryBuilder::<Sqlite>::new("SELECT status, media_kind, COUNT(*) AS n FROM requests");
        ContentFilter {
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
    pub async fn save(&self, r: &ContentRow) -> Result<()> {
        sqlx::query(
            "UPDATE requests SET book_formats = ?, status = ?, profile_id = ?, root_folder_id = ?, tags = ?,
                modified_by = ?, decline_reason = ?, failure_reason = ?, book_id = ?, author_id = ?, album_id = ?,
                artist_id = ?, approved_at = ?, available_at = ?, updated_at = ?
             WHERE id = ? AND media_kind IN ('book', 'music')",
        )
        .bind(json(&r.formats))
        .bind(r.status.as_str())
        .bind(r.profile_id)
        .bind(r.root_folder_id)
        .bind(json(&r.tags))
        .bind(r.modified_by)
        .bind(&r.decline_reason)
        .bind(&r.failure_reason)
        .bind(r.book_id)
        .bind(r.author_id)
        .bind(r.album_id)
        .bind(r.artist_id)
        .bind(r.approved_at)
        .bind(r.available_at)
        .bind(now_ms())
        .bind(r.id)
        .execute(self.0.pool())
        .await?;
        Ok(())
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        let r = sqlx::query("DELETE FROM requests WHERE id = ? AND media_kind IN ('book', 'music')")
            .bind(id)
            .execute(self.0.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Non-declined requests of `user_id` and `kind` created at or after `since_ms`.
    pub async fn count_since(&self, user_id: i64, kind: ContentRequestKind, since_ms: i64) -> Result<u32> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM requests WHERE user_id = ? AND media_kind = ? AND created_at >= ?
                AND status != 'declined'",
        )
        .bind(user_id)
        .bind(kind.as_str())
        .bind(since_ms)
        .fetch_one(self.0.pool())
        .await?;
        Ok(n.max(0) as u32)
    }
}
