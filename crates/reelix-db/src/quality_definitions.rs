//! Quality definitions (size limits per quality); seeded with the defaults on first access.
//!
//! Every family shares the table (keyed by quality id). Sizes are MB per minute, except for
//! ebooks where they are MB per book (see `reelix_decision::SizeBasis`).

use reelix_decision::{QualityDefinition, default_definitions_for};
use reelix_parser::{Quality, QualityFamily};
use sqlx::Row;

use crate::{Db, DbError, Result, decode_err, now_ms};

pub struct QualityDefinitionRepo<'a> {
    pub(crate) db: &'a Db,
}

/// Claims a one-time seeding marker in `settings` inside `tx`; true if this call won it.
pub(crate) async fn claim_seed(tx: &mut sqlx::SqliteConnection, key: &str) -> Result<bool> {
    let res = sqlx::query("INSERT OR IGNORE INTO settings (key, value, updated_at) VALUES (?, 'true', ?)")
        .bind(format!("seeded.{key}"))
        .bind(now_ms())
        .execute(&mut *tx)
        .await?;
    Ok(res.rows_affected() == 1)
}

impl QualityDefinitionRepo<'_> {
    /// Inserts defaults for any quality of `family` without a row (idempotent; also covers new
    /// qualities).
    async fn ensure(&self, family: QualityFamily) -> Result<()> {
        let now = now_ms();
        let mut tx = self.db.pool().begin().await?;
        for d in default_definitions_for(family) {
            sqlx::query(
                "INSERT OR IGNORE INTO quality_definitions (quality_id, title, min_size, preferred_size, max_size, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(d.quality.id() as i64)
            .bind(&d.title)
            .bind(d.min_size)
            .bind(d.preferred_size)
            .bind(d.max_size)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Video definitions in quality ranking order.
    pub async fn list(&self) -> Result<Vec<QualityDefinition>> {
        self.list_family(QualityFamily::Video).await
    }

    /// Definitions of one family in its ranking order (non-video rows share the table).
    pub async fn list_family(&self, family: QualityFamily) -> Result<Vec<QualityDefinition>> {
        self.ensure(family).await?;
        let rows = sqlx::query(
            "SELECT quality_id, title, min_size, preferred_size, max_size FROM quality_definitions",
        )
        .fetch_all(self.db.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: i64 = r.try_get("quality_id")?;
            let Some(quality) = u16::try_from(id).ok().and_then(Quality::from_id) else {
                continue; // a quality removed from the model: ignore its row
            };
            if quality.family() != family {
                continue;
            }
            out.push(QualityDefinition {
                quality,
                title: r.try_get("title")?,
                min_size: r.try_get("min_size")?,
                preferred_size: r.try_get("preferred_size")?,
                max_size: r.try_get("max_size")?,
            });
        }
        let order = family.qualities();
        out.sort_by_key(|d| order.iter().position(|q| *q == d.quality));
        Ok(out)
    }

    /// Bulk update (any family); every definition must be valid. Returns the full video list.
    pub async fn update_all(&self, defs: &[QualityDefinition]) -> Result<Vec<QualityDefinition>> {
        for d in defs {
            d.validate().map_err(DbError::Conflict)?;
        }
        for f in QualityFamily::ALL {
            if f == QualityFamily::Video || defs.iter().any(|d| d.quality.family() == f) {
                self.ensure(f).await?;
            }
        }
        let now = now_ms();
        let mut tx = self.db.pool().begin().await?;
        for d in defs {
            sqlx::query(
                "UPDATE quality_definitions SET title = ?, min_size = ?, preferred_size = ?, max_size = ?, updated_at = ?
                 WHERE quality_id = ?",
            )
            .bind(d.title.trim())
            .bind(d.min_size)
            .bind(d.preferred_size)
            .bind(d.max_size)
            .bind(now)
            .bind(d.quality.id() as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.list().await
    }

    /// Bulk update of one family's definitions; rejects definitions of other families. Returns
    /// the family's full list.
    pub async fn update_family(
        &self,
        family: QualityFamily,
        defs: &[QualityDefinition],
    ) -> Result<Vec<QualityDefinition>> {
        if let Some(d) = defs.iter().find(|d| d.quality.family() != family) {
            return Err(DbError::Conflict(format!(
                "{} is not a {} quality",
                d.quality.name(),
                family.as_str()
            )));
        }
        self.update_all(defs).await?;
        self.list_family(family).await
    }

    /// Restores the default table.
    pub async fn reset(&self) -> Result<Vec<QualityDefinition>> {
        sqlx::query("DELETE FROM quality_definitions")
            .execute(self.db.pool())
            .await?;
        self.list().await
    }
}

/// Decodes a JSON column.
pub(crate) fn json_col<T: serde::de::DeserializeOwned>(
    row: &sqlx::sqlite::SqliteRow,
    col: &str,
) -> Result<T> {
    let raw: String = row.try_get(col)?;
    serde_json::from_str(&raw).map_err(|e| decode_err(format!("{col}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelix_decision::default_definitions;

    #[tokio::test]
    async fn families_share_the_table() {
        let db = Db::in_memory().await.unwrap();
        let repo = db.quality_definitions();
        let music = repo.list_family(QualityFamily::Music).await.unwrap();
        assert_eq!(music.len(), Quality::MUSIC.len());
        assert_eq!(music[0].quality, Quality::UnknownMusic);
        assert_eq!(repo.list().await.unwrap(), default_definitions());
        let mut flac = music.iter().find(|d| d.quality == Quality::Flac).unwrap().clone();
        flac.max_size = Some(12.0);
        repo.update_all(std::slice::from_ref(&flac)).await.unwrap();
        let again = repo.list_family(QualityFamily::Music).await.unwrap();
        assert!(again.contains(&flac));
    }

    #[tokio::test]
    async fn seeded_and_updatable() {
        let db = Db::in_memory().await.unwrap();
        let mut defs = db.quality_definitions().list().await.unwrap();
        assert_eq!(defs.len(), Quality::ALL.len());
        assert_eq!(defs[0].quality, Quality::Unknown);
        let d = defs
            .iter_mut()
            .find(|d| d.quality == Quality::Bluray1080p)
            .unwrap();
        d.max_size = Some(300.0);
        d.title = "Blu-ray 1080p".into();
        let updated = db.quality_definitions().update_all(&defs).await.unwrap();
        let b = updated
            .iter()
            .find(|d| d.quality == Quality::Bluray1080p)
            .unwrap();
        assert_eq!((b.max_size, b.title.as_str()), (Some(300.0), "Blu-ray 1080p"));
        let mut bad = updated.clone();
        bad[5].min_size = -1.0;
        assert!(matches!(
            db.quality_definitions().update_all(&bad).await,
            Err(DbError::Conflict(_))
        ));
        let reset = db.quality_definitions().reset().await.unwrap();
        assert_eq!(reset, default_definitions());
    }

    #[tokio::test]
    async fn family_defaults_and_update_family() {
        use reelix_decision::default_definitions_for;
        let db = Db::in_memory().await.unwrap();
        let repo = db.quality_definitions();
        for f in QualityFamily::ALL {
            assert_eq!(repo.list_family(f).await.unwrap(), default_definitions_for(f));
        }
        let mut epub = repo
            .list_family(QualityFamily::Ebook)
            .await
            .unwrap()
            .into_iter()
            .find(|d| d.quality == Quality::Epub)
            .unwrap();
        epub.max_size = Some(50.0);
        let list = repo
            .update_family(QualityFamily::Ebook, std::slice::from_ref(&epub))
            .await
            .unwrap();
        assert_eq!(list.len(), Quality::EBOOK.len());
        assert!(list.contains(&epub));
        // Wrong family is refused, and video is untouched.
        assert!(matches!(
            repo.update_family(QualityFamily::Music, std::slice::from_ref(&epub))
                .await,
            Err(DbError::Conflict(_))
        ));
        assert_eq!(repo.list().await.unwrap(), default_definitions());
        // Reset restores every family's defaults.
        repo.reset().await.unwrap();
        assert_eq!(
            repo.list_family(QualityFamily::Ebook).await.unwrap(),
            default_definitions_for(QualityFamily::Ebook)
        );
    }
}
