//! SQL for the `providers`, `indexer_status`, `tags` and `remote_path_mappings` tables.
//! Settings are passed in/out as stored (secrets still encrypted); see `crypto`.

use reelix_api_types::providers::ProviderKind;
use reelix_db::{Db, DbError, from_ms, now_ms};
use reelix_indexers::IndexerStatus;
use serde_json::{Map, Value};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::error::{ProviderError, Result};
use crate::model::{ProviderRow, RemotePathMapping, Tag};

fn unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_unique_violation())
}

fn map_write(e: sqlx::Error, what: &str) -> ProviderError {
    if unique_violation(&e) {
        ProviderError::Conflict(format!("{what} already exists"))
    } else {
        e.into()
    }
}

/// Decodes a row; settings stay as stored (secrets encrypted).
fn decode(r: &SqliteRow) -> Result<ProviderRow> {
    let kind: String = r.try_get("kind")?;
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ProviderError::Db(DbError::Conflict(format!("unknown provider kind `{kind}`"))))?;
    let tags: String = r.try_get("tags")?;
    let settings: String = r.try_get("settings")?;
    let settings: Map<String, Value> = serde_json::from_str(&settings)?;
    Ok(ProviderRow {
        id: r.try_get("id")?,
        kind,
        implementation: r.try_get("implementation")?,
        name: r.try_get("name")?,
        enabled: r.try_get::<i64, _>("enabled")? != 0,
        priority: r.try_get::<i64, _>("priority")? as i32,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        settings,
        created_at: from_ms(r.try_get("created_at")?),
        updated_at: from_ms(r.try_get("updated_at")?),
    })
}

const COLS: &str =
    "id, kind, implementation, name, enabled, priority, tags, settings, created_at, updated_at";

pub(crate) async fn list(db: &Db, kind: Option<ProviderKind>) -> Result<Vec<ProviderRow>> {
    let rows = match kind {
        Some(k) => {
            sqlx::query(&format!(
                "SELECT {COLS} FROM providers WHERE kind = ? ORDER BY priority, name COLLATE NOCASE, id"
            ))
            .bind(k.as_str())
            .fetch_all(db.pool())
            .await?
        }
        None => {
            sqlx::query(&format!(
                "SELECT {COLS} FROM providers ORDER BY kind, priority, name COLLATE NOCASE, id"
            ))
            .fetch_all(db.pool())
            .await?
        }
    };
    rows.iter().map(decode).collect()
}

pub(crate) async fn get(db: &Db, kind: ProviderKind, id: i64) -> Result<Option<ProviderRow>> {
    let row = sqlx::query(&format!("SELECT {COLS} FROM providers WHERE id = ? AND kind = ?"))
        .bind(id)
        .bind(kind.as_str())
        .fetch_optional(db.pool())
        .await?;
    row.as_ref().map(decode).transpose()
}

pub(crate) struct WriteProvider<'a> {
    pub kind: ProviderKind,
    pub implementation: &'a str,
    pub name: &'a str,
    pub enabled: bool,
    pub priority: i32,
    pub tags: &'a [i64],
    /// Already encrypted.
    pub settings: &'a Map<String, Value>,
}

pub(crate) async fn insert(db: &Db, w: &WriteProvider<'_>) -> Result<i64> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO providers (kind, implementation, name, enabled, priority, tags, settings, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(w.kind.as_str())
    .bind(w.implementation)
    .bind(w.name)
    .bind(w.enabled)
    .bind(w.priority)
    .bind(serde_json::to_string(w.tags)?)
    .bind(serde_json::to_string(w.settings)?)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| map_write(e, &format!("{} named `{}`", w.kind.label(), w.name)))?;
    Ok(res.last_insert_rowid())
}

pub(crate) async fn update(db: &Db, id: i64, w: &WriteProvider<'_>) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE providers SET implementation = ?, name = ?, enabled = ?, priority = ?, tags = ?, settings = ?,
         updated_at = ? WHERE id = ? AND kind = ?",
    )
    .bind(w.implementation)
    .bind(w.name)
    .bind(w.enabled)
    .bind(w.priority)
    .bind(serde_json::to_string(w.tags)?)
    .bind(serde_json::to_string(w.settings)?)
    .bind(now_ms())
    .bind(id)
    .bind(w.kind.as_str())
    .execute(db.pool())
    .await
    .map_err(|e| map_write(e, &format!("{} named `{}`", w.kind.label(), w.name)))?;
    Ok(res.rows_affected() > 0)
}

/// Rewrites only the stored settings (used to seal secrets in place; `updated_at` is kept).
pub(crate) async fn update_settings(db: &Db, id: i64, settings: &Map<String, Value>) -> Result<()> {
    sqlx::query("UPDATE providers SET settings = ? WHERE id = ?")
        .bind(serde_json::to_string(settings)?)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub(crate) async fn delete(db: &Db, kind: ProviderKind, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM providers WHERE id = ? AND kind = ?")
        .bind(id)
        .bind(kind.as_str())
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}

// ---------- indexer status ----------

pub(crate) async fn load_indexer_status(db: &Db) -> Result<Vec<(i64, IndexerStatus)>> {
    let rows = sqlx::query(
        "SELECT provider_id, failure_count, disabled_until, last_failure_reason FROM indexer_status",
    )
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get("provider_id")?,
                IndexerStatus {
                    failure_count: r.try_get::<i64, _>("failure_count")?.max(0) as u32,
                    disabled_until: r.try_get::<Option<i64>, _>("disabled_until")?.map(from_ms),
                    last_failure_reason: r.try_get("last_failure_reason")?,
                },
            ))
        })
        .collect()
}

pub(crate) async fn save_indexer_status(db: &Db, id: i64, s: &IndexerStatus) -> Result<()> {
    sqlx::query(
        "INSERT INTO indexer_status (provider_id, failure_count, disabled_until, last_failure_reason, updated_at)
         SELECT ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM providers WHERE id = ?)
         ON CONFLICT(provider_id) DO UPDATE SET failure_count = excluded.failure_count,
            disabled_until = excluded.disabled_until, last_failure_reason = excluded.last_failure_reason,
            updated_at = excluded.updated_at",
    )
    .bind(id)
    .bind(i64::from(s.failure_count))
    .bind(s.disabled_until.map(|d| d.timestamp_millis()))
    .bind(&s.last_failure_reason)
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

// ---------- tags ----------

pub(crate) async fn list_tags(db: &Db) -> Result<Vec<Tag>> {
    let rows = sqlx::query("SELECT id, label FROM tags ORDER BY label COLLATE NOCASE")
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| {
            Ok(Tag {
                id: r.try_get("id")?,
                label: r.try_get("label")?,
            })
        })
        .collect()
}

pub(crate) async fn create_tag(db: &Db, label: &str) -> Result<Tag> {
    let res = sqlx::query("INSERT INTO tags (label, created_at) VALUES (?, ?)")
        .bind(label)
        .bind(now_ms())
        .execute(db.pool())
        .await
        .map_err(|e| map_write(e, &format!("tag `{label}`")))?;
    Ok(Tag {
        id: res.last_insert_rowid(),
        label: label.to_owned(),
    })
}

pub(crate) async fn delete_tag(db: &Db, id: i64) -> Result<bool> {
    let mut tx = db.pool().begin().await?;
    let rows = sqlx::query("SELECT id, tags FROM providers WHERE tags LIKE ?")
        .bind(format!("%{id}%"))
        .fetch_all(&mut *tx)
        .await?;
    for r in rows {
        let pid: i64 = r.try_get("id")?;
        let tags: String = r.try_get("tags")?;
        let mut tags: Vec<i64> = serde_json::from_str(&tags).unwrap_or_default();
        let before = tags.len();
        tags.retain(|t| *t != id);
        if tags.len() != before {
            sqlx::query("UPDATE providers SET tags = ? WHERE id = ?")
                .bind(serde_json::to_string(&tags)?)
                .bind(pid)
                .execute(&mut *tx)
                .await?;
        }
    }
    let res = sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(res.rows_affected() > 0)
}

// ---------- remote path mappings ----------

pub(crate) async fn list_mappings(db: &Db) -> Result<Vec<RemotePathMapping>> {
    let rows = sqlx::query(
        "SELECT id, host, remote_path, local_path FROM remote_path_mappings ORDER BY host COLLATE NOCASE, remote_path",
    )
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| {
            Ok(RemotePathMapping {
                id: r.try_get("id")?,
                host: r.try_get("host")?,
                remote_path: r.try_get("remote_path")?,
                local_path: r.try_get("local_path")?,
            })
        })
        .collect()
}

pub(crate) async fn create_mapping(db: &Db, host: &str, remote: &str, local: &str) -> Result<i64> {
    let res = sqlx::query(
        "INSERT INTO remote_path_mappings (host, remote_path, local_path, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(host)
    .bind(remote)
    .bind(local)
    .bind(now_ms())
    .execute(db.pool())
    .await
    .map_err(|e| map_write(e, "a mapping for this host and remote path"))?;
    Ok(res.last_insert_rowid())
}

pub(crate) async fn update_mapping(db: &Db, id: i64, host: &str, remote: &str, local: &str) -> Result<bool> {
    let res =
        sqlx::query("UPDATE remote_path_mappings SET host = ?, remote_path = ?, local_path = ? WHERE id = ?")
            .bind(host)
            .bind(remote)
            .bind(local)
            .bind(id)
            .execute(db.pool())
            .await
            .map_err(|e| map_write(e, "a mapping for this host and remote path"))?;
    Ok(res.rows_affected() > 0)
}

pub(crate) async fn delete_mapping(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM remote_path_mappings WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}
