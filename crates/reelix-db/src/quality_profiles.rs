//! Quality profiles; the built-in profiles of each family are seeded once, on first access of
//! that family (`reelix_decision::default_profiles_for`).
//!
//! Profiles belong to one quality family (`media_kind`). `list`/`get` only see video profiles,
//! so movie/series consumers can never pick a book or music profile.

use reelix_decision::{QualityProfile, default_profiles_for};
use reelix_parser::{Language, QualityFamily};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::error::is_unique_violation;
use crate::quality_definitions::{claim_seed, json_col};
use crate::{Db, DbError, Result, now_ms};

pub struct QualityProfileRepo<'a> {
    pub(crate) db: &'a Db,
}

const COLS: &str = "id, name, items, cutoff, upgrade_allowed, min_format_score, cutoff_format_score, \
                    min_upgrade_format_score, format_items, language, media_kind";

fn map(row: &SqliteRow) -> Result<QualityProfile> {
    let language: Option<i64> = row.try_get("language")?;
    let kind: String = row.try_get("media_kind")?;
    let family = QualityFamily::parse(&kind)
        .ok_or_else(|| crate::decode_err(format!("unknown quality profile kind {kind:?}")))?;
    Ok(QualityProfile {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        items: json_col(row, "items")?,
        cutoff: row.try_get::<i64, _>("cutoff")? as u32,
        upgrade_allowed: row.try_get("upgrade_allowed")?,
        min_format_score: row.try_get("min_format_score")?,
        cutoff_format_score: row.try_get("cutoff_format_score")?,
        min_upgrade_format_score: row.try_get("min_upgrade_format_score")?,
        format_items: json_col(row, "format_items")?,
        language: language
            .and_then(|l| u16::try_from(l).ok())
            .and_then(Language::from_id),
        family,
    })
}

fn conflict(e: sqlx::Error, name: &str) -> DbError {
    if is_unique_violation(&e) {
        DbError::Conflict(format!("a quality profile named {name:?} already exists"))
    } else {
        e.into()
    }
}

async fn insert(
    conn: &mut sqlx::SqliteConnection,
    p: &QualityProfile,
) -> std::result::Result<i64, sqlx::Error> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO quality_profiles (name, items, cutoff, upgrade_allowed, min_format_score, cutoff_format_score,
             min_upgrade_format_score, format_items, language, media_kind, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(p.name.trim())
    .bind(serde_json::to_string(&p.items).unwrap_or_else(|_| "[]".into()))
    .bind(p.cutoff as i64)
    .bind(p.upgrade_allowed)
    .bind(p.min_format_score)
    .bind(p.cutoff_format_score)
    .bind(p.min_upgrade_format_score)
    .bind(serde_json::to_string(&p.format_items).unwrap_or_else(|_| "[]".into()))
    .bind(p.language.map(|l| l.id() as i64))
    .bind(p.family.as_str())
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(res.last_insert_rowid())
}

impl QualityProfileRepo<'_> {
    /// Seeds the built-in profiles of `family` once (video keeps its historical marker).
    async fn ensure_family(&self, family: QualityFamily) -> Result<()> {
        let key = match family {
            QualityFamily::Video => "quality_profiles".to_string(),
            f => format!("quality_profiles.{}", f.as_str()),
        };
        let mut tx = self.db.pool().begin().await?;
        if claim_seed(&mut tx, &key).await? {
            for p in default_profiles_for(family) {
                insert(&mut tx, &QualityProfile { family, ..p }).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn ensure(&self) -> Result<()> {
        self.ensure_family(QualityFamily::Video).await
    }

    /// Video profiles only.
    pub async fn list(&self) -> Result<Vec<QualityProfile>> {
        self.list_family(QualityFamily::Video).await
    }

    /// Profiles of one family (seeding its built-in profiles on first access).
    pub async fn list_family(&self, family: QualityFamily) -> Result<Vec<QualityProfile>> {
        self.ensure_family(family).await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM quality_profiles WHERE media_kind = ? ORDER BY id"
        ))
        .bind(family.as_str())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(map).collect()
    }

    /// Profiles of every family.
    pub async fn list_all(&self) -> Result<Vec<QualityProfile>> {
        for f in QualityFamily::ALL {
            self.ensure_family(f).await?;
        }
        let rows = sqlx::query(&format!("SELECT {COLS} FROM quality_profiles ORDER BY id"))
            .fetch_all(self.db.pool())
            .await?;
        rows.iter().map(map).collect()
    }

    /// A video profile; `None` for missing ids and for profiles of other families.
    pub async fn get(&self, id: i64) -> Result<Option<QualityProfile>> {
        Ok(self
            .get_any(id)
            .await?
            .filter(|p| p.family == QualityFamily::Video))
    }

    /// A profile of any family.
    pub async fn get_any(&self, id: i64) -> Result<Option<QualityProfile>> {
        self.ensure().await?;
        let row = sqlx::query(&format!("SELECT {COLS} FROM quality_profiles WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        row.as_ref().map(map).transpose()
    }

    /// Inserts `p` (its `id` is ignored); `Conflict` on a duplicate name.
    pub async fn create(&self, p: &QualityProfile) -> Result<QualityProfile> {
        self.ensure().await?;
        let mut conn = self.db.pool().acquire().await?;
        let id = insert(&mut conn, p).await.map_err(|e| conflict(e, &p.name))?;
        drop(conn);
        Ok(self.get_any(id).await?.unwrap_or_else(|| p.clone()))
    }

    /// Replaces profile `id` (any family; the family itself is immutable); `None` if it doesn't exist.
    pub async fn update(&self, id: i64, p: &QualityProfile) -> Result<Option<QualityProfile>> {
        let res = sqlx::query(
            "UPDATE quality_profiles SET name = ?, items = ?, cutoff = ?, upgrade_allowed = ?, min_format_score = ?,
                 cutoff_format_score = ?, min_upgrade_format_score = ?, format_items = ?, language = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(p.name.trim())
        .bind(serde_json::to_string(&p.items)?)
        .bind(p.cutoff as i64)
        .bind(p.upgrade_allowed)
        .bind(p.min_format_score)
        .bind(p.cutoff_format_score)
        .bind(p.min_upgrade_format_score)
        .bind(serde_json::to_string(&p.format_items)?)
        .bind(p.language.map(|l| l.id() as i64))
        .bind(now_ms())
        .bind(id)
        .execute(self.db.pool())
        .await
        .map_err(|e| conflict(e, &p.name))?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_any(id).await
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        let res = sqlx::query("DELETE FROM quality_profiles WHERE id = ?")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// Removes a deleted custom format's score from every profile.
pub(crate) async fn remove_format_scores(conn: &mut sqlx::SqliteConnection, format_id: i64) -> Result<()> {
    let rows = sqlx::query("SELECT id, format_items FROM quality_profiles")
        .fetch_all(&mut *conn)
        .await?;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let mut items: Vec<reelix_decision::FormatItem> = json_col(&row, "format_items")?;
        let before = items.len();
        items.retain(|i| i.format_id != format_id);
        if items.len() != before {
            sqlx::query("UPDATE quality_profiles SET format_items = ?, updated_at = ? WHERE id = ?")
                .bind(serde_json::to_string(&items)?)
                .bind(now_ms())
                .bind(id)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn crud_and_seed_once() {
        let db = Db::in_memory().await.unwrap();
        let repo = db.quality_profiles();
        let all = repo.list().await.unwrap();
        assert_eq!(
            all.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Any", "SD", "HD-720p", "HD-1080p", "Ultra-HD", "HD-720p/1080p"]
        );
        let mut p = all[3].clone();
        assert!(matches!(repo.create(&p).await, Err(DbError::Conflict(_))));
        p.name = "Custom".into();
        p.min_format_score = 10;
        let created = repo.create(&p).await.unwrap();
        assert!(created.id > 6);
        assert_eq!(created.min_format_score, 10);
        assert_eq!(created.items, p.items);
        p.name = "Custom 2".into();
        let updated = repo.update(created.id, &p).await.unwrap().unwrap();
        assert_eq!(updated.name, "Custom 2");
        assert!(repo.update(9999, &p).await.unwrap().is_none());
        // Deleting everything must not re-seed.
        for p in repo.list().await.unwrap() {
            assert!(repo.delete(p.id).await.unwrap());
        }
        assert!(repo.list().await.unwrap().is_empty());
        assert!(!repo.delete(1).await.unwrap());
    }

    #[tokio::test]
    async fn families_are_separate() {
        let db = Db::in_memory().await.unwrap();
        let repo = db.quality_profiles();
        let video = repo.list().await.unwrap();
        let mut p = QualityProfile::with_allowed("Books", &[], reelix_parser::Quality::Unknown);
        p.family = QualityFamily::Ebook;
        let book = repo.create(&p).await.unwrap();
        assert_eq!(book.family, QualityFamily::Ebook);
        assert_eq!(repo.list().await.unwrap(), video);
        assert!(repo.get(book.id).await.unwrap().is_none());
        assert_eq!(repo.get_any(book.id).await.unwrap().unwrap().name, "Books");
        let ebook = repo.list_family(QualityFamily::Ebook).await.unwrap();
        assert!(ebook.contains(&book));
        assert!(ebook.iter().all(|p| p.family == QualityFamily::Ebook));
        let seeded: usize = QualityFamily::ALL
            .iter()
            .map(|f| reelix_decision::default_profiles_for(*f).len())
            .sum();
        assert_eq!(repo.list_all().await.unwrap().len(), seeded + 1);
        // The family survives an update that asks for another one.
        let mut edit = book.clone();
        edit.family = QualityFamily::Video;
        edit.name = "Books 2".into();
        let updated = repo.update(book.id, &edit).await.unwrap().unwrap();
        assert_eq!(updated.family, QualityFamily::Ebook);
        // Video JSON keeps its historical shape.
        assert!(!serde_json::to_string(&video[0]).unwrap().contains("family"));
    }

    #[tokio::test]
    async fn family_defaults_seed_once() {
        let db = Db::in_memory().await.unwrap();
        let repo = db.quality_profiles();
        let video = repo.list().await.unwrap();
        for f in [
            QualityFamily::Ebook,
            QualityFamily::Audiobook,
            QualityFamily::Music,
        ] {
            let seeded = repo.list_family(f).await.unwrap();
            let defaults = reelix_decision::default_profiles_for(f);
            assert_eq!(
                seeded.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
                defaults.iter().map(|p| p.name.as_str()).collect::<Vec<_>>()
            );
            for (s, d) in seeded.iter().zip(&defaults) {
                assert_eq!(
                    (s.family, &s.items, s.cutoff, s.language),
                    (f, &d.items, d.cutoff, d.language)
                );
                assert!(repo.get(s.id).await.unwrap().is_none());
            }
            for p in &seeded {
                assert!(repo.delete(p.id).await.unwrap());
            }
            assert!(repo.list_family(f).await.unwrap().is_empty(), "{f:?} re-seeded");
        }
        assert_eq!(repo.list().await.unwrap(), video);
    }
}
