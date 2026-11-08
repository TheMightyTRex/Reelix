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

/// The key, with a use not yet written (see [`crate::key_use`]) in place of an older stamp.
fn map_key(row: &SqliteRow, pending: impl Fn(&str) -> Option<i64>) -> Result<ApiKey> {
    let stored: Option<i64> = row.try_get("last_used_at")?;
    let hash: String = row.try_get("key_hash")?;
    let last = match (stored, pending(&hash)) {
        (Some(s), Some(p)) => Some(s.max(p)),
        (s, p) => s.or(p),
    };
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
             RETURNING id, user_id, name, prefix, key_hash, created_at, last_used_at",
        )
        .bind(user_id)
        .bind(name)
        .bind(prefix)
        .bind(key_hash)
        .bind(now_ms())
        .fetch_one(self.db.pool())
        .await?;
        map_key(&row, |_| None)
    }

    pub async fn list_for_user(&self, user_id: i64) -> Result<Vec<ApiKey>> {
        let rows = sqlx::query(
            "SELECT id, user_id, name, prefix, key_hash, created_at, last_used_at FROM api_keys
             WHERE user_id = ? ORDER BY id",
        )
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await?;
        rows.iter()
            .map(|r| map_key(r, |h| self.db.key_use.pending(h)))
            .collect()
    }

    /// Deletes one of the user's keys; `false` if it didn't exist or belongs to someone else.
    pub async fn delete(&self, user_id: i64, id: i64) -> Result<bool> {
        let hash: Option<String> =
            sqlx::query_scalar("DELETE FROM api_keys WHERE id = ? AND user_id = ? RETURNING key_hash")
                .bind(id)
                .bind(user_id)
                .fetch_optional(self.db.pool())
                .await?;
        if let Some(hash) = &hash {
            self.db.key_use.forget(hash);
        }
        Ok(hash.is_some())
    }

    /// The key's owner. Only reads: the use is noted in memory and written by
    /// [`Db::flush_key_use`] at most once a minute, so a request with a key never waits for the
    /// write lock (RFX-71).
    pub async fn find_user(&self, key_hash: &str) -> Result<Option<User>> {
        let user_id: Option<i64> = sqlx::query_scalar("SELECT user_id FROM api_keys WHERE key_hash = ?")
            .bind(key_hash)
            .fetch_optional(self.db.pool())
            .await?;
        let Some(user_id) = user_id else {
            return Ok(None);
        };
        self.db.key_use.record(key_hash, now_ms());
        self.db.users().get(user_id).await
    }
}

#[cfg(test)]
mod tests {
    use crate::Db;
    use crate::users::tests::new_user;

    async fn stored(db: &Db, hash: &str) -> Option<i64> {
        sqlx::query_scalar("SELECT last_used_at FROM api_keys WHERE key_hash = ?")
            .bind(hash)
            .fetch_one(db.pool())
            .await
            .unwrap()
    }

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
        assert!(
            listed[0].last_used_at.is_some(),
            "a use shows before it is written"
        );
        assert!(db.api_keys().list_for_user(bob.id).await.unwrap().is_empty());
        assert!(db.api_keys().find_user("nope").await.unwrap().is_none());

        assert!(!db.api_keys().delete(bob.id, key.id).await.unwrap());
        assert!(db.api_keys().delete(alice.id, key.id).await.unwrap());
        assert!(!db.api_keys().delete(alice.id, key.id).await.unwrap());
        assert!(db.api_keys().find_user("h1").await.unwrap().is_none());
        assert_eq!(
            db.flush_key_use().await.unwrap(),
            0,
            "a deleted key's use is forgotten"
        );
    }

    /// RFX-71: requests with a key write nothing; one flush writes each key's latest use once.
    #[tokio::test]
    async fn key_use_is_written_by_the_flush_not_by_requests() {
        let db = Db::in_memory().await.unwrap();
        let alice = db.users().create(new_user("alice")).await.unwrap();
        db.api_keys().create(alice.id, "a", "aaaa", "ha").await.unwrap();
        db.api_keys().create(alice.id, "b", "bbbb", "hb").await.unwrap();

        for _ in 0..50 {
            db.api_keys().find_user("ha").await.unwrap().unwrap();
        }
        db.api_keys().find_user("hb").await.unwrap().unwrap();
        assert_eq!(stored(&db, "ha").await, None, "a request wrote last_used_at");
        assert_eq!(stored(&db, "hb").await, None);

        assert_eq!(db.flush_key_use().await.unwrap(), 2);
        let a = stored(&db, "ha").await.expect("written by the flush");
        assert!(stored(&db, "hb").await.is_some());
        assert_eq!(db.flush_key_use().await.unwrap(), 0, "nothing new to write");

        // A later use moves the stamp on; an earlier stamp never moves it back.
        sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE key_hash = 'hb'")
            .bind(i64::MAX / 2)
            .execute(db.pool())
            .await
            .unwrap();
        db.api_keys().find_user("ha").await.unwrap().unwrap();
        db.api_keys().find_user("hb").await.unwrap().unwrap();
        assert_eq!(db.flush_key_use().await.unwrap(), 2);
        assert!(stored(&db, "ha").await.unwrap() >= a);
        assert_eq!(stored(&db, "hb").await, Some(i64::MAX / 2));
    }

    /// RFX-71: with the write lock held elsewhere for longer than the busy timeout, a request
    /// with a key still answers at once, and a flush that cannot write keeps the use for later.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn key_requests_do_not_wait_for_the_write_lock() {
        use sqlx::Connection;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("reelix.db");
        let db = Db::connect(&format!("sqlite://{}", file.display()))
            .await
            .unwrap();
        let alice = db.users().create(new_user("alice")).await.unwrap();
        db.api_keys().create(alice.id, "a", "aaaa", "ha").await.unwrap();

        let mut other = sqlx::SqliteConnection::connect(&format!("sqlite://{}", file.display()))
            .await
            .unwrap();
        sqlx::query("BEGIN IMMEDIATE").execute(&mut other).await.unwrap();
        sqlx::query("UPDATE users SET updated_at = updated_at")
            .execute(&mut other)
            .await
            .unwrap();

        let started = std::time::Instant::now();
        for _ in 0..20 {
            db.api_keys().find_user("ha").await.unwrap().unwrap();
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "requests waited {:?} for the write lock",
            started.elapsed()
        );
        // The flush cannot write while the lock is held (it waits the busy timeout, then fails)
        // and keeps the use.
        assert!(db.flush_key_use().await.is_err());
        sqlx::query("COMMIT").execute(&mut other).await.unwrap();
        assert_eq!(
            db.flush_key_use().await.unwrap(),
            1,
            "the use was kept for the next flush"
        );
        assert!(stored(&db, "ha").await.is_some());
    }
}
