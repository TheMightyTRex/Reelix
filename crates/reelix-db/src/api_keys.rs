use chrono::{DateTime, Utc};
use reelix_core::User;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Db, Result, from_ms, now_ms};

/// Stored API key metadata (the key itself is only kept as a hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKey {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

fn map_key(row: &SqliteRow) -> Result<ApiKey> {
    let last: Option<i64> = row.try_get("last_used_at")?;
    Ok(ApiKey {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        name: row.try_get("name")?,
        prefix: row.try_get("prefix")?,
        created_at: from_ms(row.try_get("created_at")?),
        last_used_at: last.map(from_ms),
    })
}

/// API keys. `key_hash` is the hex sha256 of the raw key.
pub struct ApiKeyRepo<'a> {
    pub(crate) db: &'a Db,
}

impl ApiKeyRepo<'_> {
    pub async fn create(&self, user_id: i64, name: &str, prefix: &str, key_hash: &str) -> Result<ApiKey> {
        let row = sqlx::query(
            "INSERT INTO api_keys (user_id, name, prefix, key_hash, created_at) VALUES (?, ?, ?, ?, ?)
             RETURNING id, user_id, name, prefix, created_at, last_used_at",
        )
        .bind(user_id)
        .bind(name)
        .bind(prefix)
        .bind(key_hash)
        .bind(now_ms())
        .fetch_one(self.db.pool())
        .await?;
        map_key(&row)
    }

    pub async fn list_for_user(&self, user_id: i64) -> Result<Vec<ApiKey>> {
        let rows = sqlx::query(
            "SELECT id, user_id, name, prefix, created_at, last_used_at FROM api_keys WHERE user_id = ? ORDER BY id",
        )
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(map_key).collect()
    }

    /// Deletes one of the user's keys; `false` if it didn't exist or belongs to someone else.
    pub async fn delete(&self, user_id: i64, id: i64) -> Result<bool> {
        let r = sqlx::query("DELETE FROM api_keys WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// The key's owner; also stamps `last_used_at = now`.
    pub async fn find_user(&self, key_hash: &str) -> Result<Option<User>> {
        let user_id: Option<i64> =
            sqlx::query_scalar("UPDATE api_keys SET last_used_at = ? WHERE key_hash = ? RETURNING user_id")
                .bind(now_ms())
                .bind(key_hash)
                .fetch_optional(self.db.pool())
                .await?;
        match user_id {
            Some(id) => self.db.users().get(id).await,
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Db;
    use crate::users::tests::new_user;

    #[tokio::test]
    async fn api_key_lifecycle() {
        let db = Db::in_memory().await.unwrap();
        let alice = db.users().create(new_user("alice")).await.unwrap();
        let bob = db.users().create(new_user("bob")).await.unwrap();

        let key = db
            .api_keys()
            .create(alice.id, "ci", "abcd1234", "h1")
            .await
            .unwrap();
        assert_eq!(key.user_id, alice.id);
        assert_eq!(key.prefix, "abcd1234");
        assert!(key.last_used_at.is_none());

        let found = db.api_keys().find_user("h1").await.unwrap().unwrap();
        assert_eq!(found.id, alice.id);
        let listed = db.api_keys().list_for_user(alice.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].last_used_at.is_some());
        assert!(db.api_keys().list_for_user(bob.id).await.unwrap().is_empty());
        assert!(db.api_keys().find_user("nope").await.unwrap().is_none());

        assert!(!db.api_keys().delete(bob.id, key.id).await.unwrap());
        assert!(db.api_keys().delete(alice.id, key.id).await.unwrap());
        assert!(!db.api_keys().delete(alice.id, key.id).await.unwrap());
        assert!(db.api_keys().find_user("h1").await.unwrap().is_none());
    }
}
