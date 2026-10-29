//! Custom formats (specifications stored as JSON).

use reelix_decision::{CustomFormat, FormatSpecification};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::error::is_unique_violation;
use crate::quality_definitions::json_col;
use crate::quality_profiles::remove_format_scores;
use crate::{Db, DbError, Result, now_ms};

pub struct CustomFormatRepo<'a> {
    pub(crate) db: &'a Db,
}

fn map(row: &SqliteRow) -> Result<CustomFormat> {
    Ok(CustomFormat {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        include_in_rename: row.try_get("include_in_rename")?,
        specifications: json_col::<Vec<FormatSpecification>>(row, "specifications")?,
    })
}

fn conflict(e: sqlx::Error, name: &str) -> DbError {
    if is_unique_violation(&e) {
        DbError::Conflict(format!("a custom format named {name:?} already exists"))
    } else {
        e.into()
    }
}

const COLS: &str = "id, name, include_in_rename, specifications";

impl CustomFormatRepo<'_> {
    pub async fn list(&self) -> Result<Vec<CustomFormat>> {
        let rows = sqlx::query(&format!("SELECT {COLS} FROM custom_formats ORDER BY name"))
            .fetch_all(self.db.pool())
            .await?;
        rows.iter().map(map).collect()
    }

    pub async fn get(&self, id: i64) -> Result<Option<CustomFormat>> {
        let row = sqlx::query(&format!("SELECT {COLS} FROM custom_formats WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        row.as_ref().map(map).transpose()
    }

    /// The TRaSH id a format was imported with, if any.
    pub async fn trash_id(&self, id: i64) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT trash_id FROM custom_formats WHERE id = ?")
                .bind(id)
                .fetch_optional(self.db.pool())
                .await?
                .flatten(),
        )
    }

    /// Inserts `f` (its `id` is ignored); `Conflict` on a duplicate name.
    pub async fn create(&self, f: &CustomFormat, trash_id: Option<&str>) -> Result<CustomFormat> {
        let now = now_ms();
        let res = sqlx::query(
            "INSERT INTO custom_formats (name, include_in_rename, specifications, trash_id, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(f.name.trim())
        .bind(f.include_in_rename)
        .bind(serde_json::to_string(&f.specifications)?)
        .bind(trash_id)
        .bind(now)
        .bind(now)
        .execute(self.db.pool())
        .await
        .map_err(|e| conflict(e, &f.name))?;
        let mut out = f.clone();
        out.id = res.last_insert_rowid();
        out.name = f.name.trim().to_string();
        Ok(out)
    }

    /// Replaces format `id`; `None` if it doesn't exist.
    pub async fn update(&self, id: i64, f: &CustomFormat) -> Result<Option<CustomFormat>> {
        let res = sqlx::query(
            "UPDATE custom_formats SET name = ?, include_in_rename = ?, specifications = ?, updated_at = ? WHERE id = ?",
        )
        .bind(f.name.trim())
        .bind(f.include_in_rename)
        .bind(serde_json::to_string(&f.specifications)?)
        .bind(now_ms())
        .bind(id)
        .execute(self.db.pool())
        .await
        .map_err(|e| conflict(e, &f.name))?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        self.get(id).await
    }

    /// Deletes the format and its scores in every quality profile.
    pub async fn delete(&self, id: i64) -> Result<bool> {
        let mut tx = self.db.pool().begin().await?;
        let res = sqlx::query("DELETE FROM custom_formats WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if res.rows_affected() > 0 {
            remove_format_scores(&mut tx, id).await?;
        }
        tx.commit().await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use reelix_decision::{Condition, FormatItem, FormatSpecification};

    use super::*;

    #[tokio::test]
    async fn crud_and_profile_cleanup() {
        let db = Db::in_memory().await.unwrap();
        let repo = db.custom_formats();
        let cf = CustomFormat {
            id: 0,
            name: "Remux".into(),
            include_in_rename: true,
            specifications: vec![FormatSpecification::new(
                "remux",
                Condition::QualityModifier {
                    value: reelix_decision::QualityModifier::Remux,
                },
            )],
        };
        let created = repo.create(&cf, Some("abc")).await.unwrap();
        assert_eq!(
            repo.get(created.id).await.unwrap().unwrap().specifications,
            cf.specifications
        );
        assert_eq!(repo.trash_id(created.id).await.unwrap().as_deref(), Some("abc"));
        assert!(matches!(repo.create(&cf, None).await, Err(DbError::Conflict(_))));

        let mut profile = db.quality_profiles().list().await.unwrap().remove(0);
        profile.format_items = vec![FormatItem {
            format_id: created.id,
            score: 50,
        }];
        db.quality_profiles().update(profile.id, &profile).await.unwrap();

        let mut renamed = created.clone();
        renamed.name = "Remux Tier".into();
        assert_eq!(
            repo.update(created.id, &renamed).await.unwrap().unwrap().name,
            "Remux Tier"
        );
        assert!(repo.delete(created.id).await.unwrap());
        assert!(repo.list().await.unwrap().is_empty());
        let after = db.quality_profiles().get(profile.id).await.unwrap().unwrap();
        assert!(after.format_items.is_empty());
    }
}
