//! Delay profiles; a default (untagged, no delay) profile is seeded once.

use reelix_core::Protocol;
use reelix_decision::DelayProfile;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::quality_definitions::{claim_seed, json_col};
use crate::{Db, Result, decode_err, now_ms};

pub struct DelayProfileRepo<'a> {
    pub(crate) db: &'a Db,
}

const COLS: &str = "id, enable_usenet, enable_torrent, preferred_protocol, usenet_delay, torrent_delay, \
                    bypass_if_highest_quality, bypass_if_above_custom_format_score, minimum_custom_format_score, \
                    tags, sort_order";

fn map(row: &SqliteRow) -> Result<DelayProfile> {
    let proto: String = row.try_get("preferred_protocol")?;
    Ok(DelayProfile {
        id: row.try_get("id")?,
        enable_usenet: row.try_get("enable_usenet")?,
        enable_torrent: row.try_get("enable_torrent")?,
        preferred_protocol: match proto.as_str() {
            "usenet" => Protocol::Usenet,
            "torrent" => Protocol::Torrent,
            other => return Err(decode_err(format!("unknown protocol {other:?}"))),
        },
        usenet_delay: row.try_get::<i64, _>("usenet_delay")?.max(0) as u32,
        torrent_delay: row.try_get::<i64, _>("torrent_delay")?.max(0) as u32,
        bypass_if_highest_quality: row.try_get("bypass_if_highest_quality")?,
        bypass_if_above_custom_format_score: row.try_get("bypass_if_above_custom_format_score")?,
        minimum_custom_format_score: row.try_get("minimum_custom_format_score")?,
        tags: json_col(row, "tags")?,
        order: row.try_get::<i64, _>("sort_order")? as i32,
    })
}

fn proto_str(p: Protocol) -> &'static str {
    match p {
        Protocol::Usenet => "usenet",
        Protocol::Torrent => "torrent",
    }
}

async fn insert(conn: &mut sqlx::SqliteConnection, p: &DelayProfile) -> Result<i64> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO delay_profiles (enable_usenet, enable_torrent, preferred_protocol, usenet_delay, torrent_delay,
             bypass_if_highest_quality, bypass_if_above_custom_format_score, minimum_custom_format_score, tags,
             sort_order, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(p.enable_usenet)
    .bind(p.enable_torrent)
    .bind(proto_str(p.preferred_protocol))
    .bind(p.usenet_delay as i64)
    .bind(p.torrent_delay as i64)
    .bind(p.bypass_if_highest_quality)
    .bind(p.bypass_if_above_custom_format_score)
    .bind(p.minimum_custom_format_score)
    .bind(serde_json::to_string(&p.tags)?)
    .bind(p.order as i64)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(res.last_insert_rowid())
}

impl DelayProfileRepo<'_> {
    async fn ensure(&self) -> Result<()> {
        let mut tx = self.db.pool().begin().await?;
        if claim_seed(&mut tx, "delay_profiles").await? {
            insert(&mut tx, &DelayProfile::default()).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// All profiles, by `order` then id.
    pub async fn list(&self) -> Result<Vec<DelayProfile>> {
        self.ensure().await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM delay_profiles ORDER BY sort_order, id"
        ))
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(map).collect()
    }

    pub async fn get(&self, id: i64) -> Result<Option<DelayProfile>> {
        self.ensure().await?;
        let row = sqlx::query(&format!("SELECT {COLS} FROM delay_profiles WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        row.as_ref().map(map).transpose()
    }

    pub async fn create(&self, p: &DelayProfile) -> Result<DelayProfile> {
        self.ensure().await?;
        let mut conn = self.db.pool().acquire().await?;
        let id = insert(&mut conn, p).await?;
        drop(conn);
        Ok(DelayProfile { id, ..p.clone() })
    }

    pub async fn update(&self, id: i64, p: &DelayProfile) -> Result<Option<DelayProfile>> {
        let res = sqlx::query(
            "UPDATE delay_profiles SET enable_usenet = ?, enable_torrent = ?, preferred_protocol = ?, usenet_delay = ?,
                 torrent_delay = ?, bypass_if_highest_quality = ?, bypass_if_above_custom_format_score = ?,
                 minimum_custom_format_score = ?, tags = ?, sort_order = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(p.enable_usenet)
        .bind(p.enable_torrent)
        .bind(proto_str(p.preferred_protocol))
        .bind(p.usenet_delay as i64)
        .bind(p.torrent_delay as i64)
        .bind(p.bypass_if_highest_quality)
        .bind(p.bypass_if_above_custom_format_score)
        .bind(p.minimum_custom_format_score)
        .bind(serde_json::to_string(&p.tags)?)
        .bind(p.order as i64)
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
        let res = sqlx::query("DELETE FROM delay_profiles WHERE id = ?")
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
    async fn seeded_default_and_crud() {
        let db = Db::in_memory().await.unwrap();
        let repo = db.delay_profiles();
        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].tags.is_empty());
        let p = DelayProfile {
            preferred_protocol: Protocol::Torrent,
            torrent_delay: 60,
            tags: vec![3, 4],
            order: 1,
            ..Default::default()
        };
        let created = repo.create(&p).await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0].id, created.id);
        let mut changed = created.clone();
        changed.usenet_delay = 15;
        assert_eq!(repo.update(created.id, &changed).await.unwrap().unwrap(), changed);
        assert!(repo.delete(created.id).await.unwrap());
        assert_eq!(repo.list().await.unwrap().len(), 1);
    }
}
