use reelix_core::User;

use crate::users::{USER_COLS, map_user};
use crate::{Db, Result, now_ms};

/// Login sessions. `token_hash` is the hex sha256 of the cookie token; raw tokens are never stored.
pub struct SessionRepo<'a> {
    pub(crate) db: &'a Db,
}

impl SessionRepo<'_> {
    pub async fn create(
        &self,
        token_hash: &str,
        user_id: i64,
        expires_at_ms: i64,
        user_agent: Option<&str>,
        ip: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at, user_agent, ip) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(user_id)
        .bind(now_ms())
        .bind(expires_at_ms)
        .bind(user_agent)
        .bind(ip)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// The session's user, unless the session is missing or expired.
    pub async fn find_user(&self, token_hash: &str, now_ms: i64) -> Result<Option<User>> {
        let row = sqlx::query(&format!(
            "SELECT {USER_COLS} FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = ? AND s.expires_at > ?"
        ))
        .bind(token_hash)
        .bind(now_ms)
        .fetch_optional(self.db.pool())
        .await?;
        row.as_ref().map(map_user).transpose()
    }

    pub async fn delete(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    pub async fn delete_expired(&self, now_ms: i64) -> Result<u64> {
        let r = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now_ms)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use crate::users::tests::new_user;
    use crate::{Db, now_ms};

    #[tokio::test]
    async fn sessions_expire_and_delete() {
        let db = Db::in_memory().await.unwrap();
        let u = db.users().create(new_user("alice")).await.unwrap();
        let now = now_ms();
        db.sessions()
            .create("live", u.id, now + 60_000, Some("ua"), Some("1.2.3.4"))
            .await
            .unwrap();
        db.sessions()
            .create("dead", u.id, now - 1, None, None)
            .await
            .unwrap();

        assert_eq!(
            db.sessions().find_user("live", now).await.unwrap().unwrap().id,
            u.id
        );
        assert!(db.sessions().find_user("dead", now).await.unwrap().is_none());
        assert!(db.sessions().find_user("missing", now).await.unwrap().is_none());

        assert_eq!(db.sessions().delete_expired(now).await.unwrap(), 1);
        db.sessions().delete("live").await.unwrap();
        assert!(db.sessions().find_user("live", now).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sessions_cascade_on_user_delete() {
        let db = Db::in_memory().await.unwrap();
        let u = db.users().create(new_user("alice")).await.unwrap();
        db.sessions()
            .create("t", u.id, now_ms() + 60_000, None, None)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(u.id)
            .execute(db.pool())
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
}
