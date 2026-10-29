//! Release profiles (required / ignored terms).

use reelix_decision::ReleaseProfile;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::quality_definitions::json_col;
use crate::{Db, Result, now_ms};

pub struct ReleaseProfileRepo<'a> {
    pub(crate) db: &'a Db,
}

const COLS: &str = "id, name, enabled, required, ignored, indexer_id, tags";

fn map(row: &SqliteRow) -> Result<ReleaseProfile> {
    Ok(ReleaseProfile {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        enabled: row.try_get("enabled")?,
        required: json_col(row, "required")?,
        ignored: json_col(row, "ignored")?,
        indexer_id: row.try_get("indexer_id")?,
        tags: json_col(row, "tags")?,
    })
}

impl ReleaseProfileRepo<'_> {
    pub async fn list(&self) -> Result<Vec<ReleaseProfile>> {
        let rows = sqlx::query(&format!("SELECT {COLS} FROM release_profiles ORDER BY id"))
            .fetch_all(self.db.pool())
            .await?;
        rows.iter().map(map).collect()
    }

    pub async fn get(&self, id: i64) -> Result<Option<ReleaseProfile>> {
        let row = sqlx::query(&format!("SELECT {COLS} FROM release_profiles WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        row.as_ref().map(map).transpose()
    }

    pub async fn create(&self, p: &ReleaseProfile) -> Result<ReleaseProfile> {
        let now = now_ms();
        let res = sqlx::query(
            "INSERT INTO release_profiles (name, enabled, required, ignored, indexer_id, tags, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(p.name.trim())
        .bind(p.enabled)
        .bind(serde_json::to_string(&p.required)?)
        .bind(serde_json::to_string(&p.ignored)?)
        .bind(p.indexer_id)
        .bind(serde_json::to_string(&p.tags)?)
        .bind(now)
        .bind(now)
        .execute(self.db.pool())
        .await?;
        Ok(ReleaseProfile {
            id: res.last_insert_rowid(),
            name: p.name.trim().to_string(),
            ..p.clone()
        })
    }

    pub async fn update(&self, id: i64, p: &ReleaseProfile) -> Result<Option<ReleaseProfile>> {
        let res = sqlx::query(
            "UPDATE release_profiles SET name = ?, enabled = ?, required = ?, ignored = ?, indexer_id = ?, tags = ?,
                 updated_at = ?
             WHERE id = ?",
        )
        .bind(p.name.trim())
        .bind(p.enabled)
        .bind(serde_json::to_string(&p.required)?)
        .bind(serde_json::to_string(&p.ignored)?)
        .bind(p.indexer_id)
        .bind(serde_json::to_string(&p.tags)?)
        .bind(now_ms())
        .bind(id)
        .execute(self.db.pool())
        .await?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        self.get(id).await
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        let res = sqlx::query("DELETE FROM release_profiles WHERE id = ?")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn crud() {
        let db = Db::in_memory().await.unwrap();
        let repo = db.release_profiles();
        assert!(repo.list().await.unwrap().is_empty());
        let p = ReleaseProfile {
            id: 0,
            name: " No YIFY ".into(),
            enabled: true,
            required: vec![],
            ignored: vec!["/\\bYIFY\\b/i".into()],
            indexer_id: Some(2),
            tags: vec![1],
        };
        let created = repo.create(&p).await.unwrap();
        assert_eq!(created.name, "No YIFY");
        assert_eq!(repo.get(created.id).await.unwrap().unwrap(), created);
        let mut changed = created.clone();
        changed.enabled = false;
        assert_eq!(repo.update(created.id, &changed).await.unwrap().unwrap(), changed);
        assert!(repo.update(999, &changed).await.unwrap().is_none());
        assert!(repo.delete(created.id).await.unwrap());
    }
}
