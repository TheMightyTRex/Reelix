use reelix_core::{AuthSource, Permissions, User};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::error::is_unique_violation;
use crate::{Db, DbError, Result, decode_err, from_ms, now_ms};

/// Fields for inserting a user.
#[derive(Debug, Clone)]
pub struct NewUser {
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub password_hash: Option<String>,
    pub auth_source: AuthSource,
    pub permissions: Permissions,
}

pub struct UserRepo<'a> {
    pub(crate) db: &'a Db,
}

pub(crate) const USER_COLS: &str =
    "u.id, u.username, u.email, u.display_name, u.auth_source, u.permissions, u.created_at";

pub(crate) fn map_user(row: &SqliteRow) -> Result<User> {
    let source: String = row.try_get("auth_source")?;
    let auth_source =
        AuthSource::parse(&source).ok_or_else(|| decode_err(format!("unknown auth_source {source:?}")))?;
    let bits: i64 = row.try_get("permissions")?;
    Ok(User {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        email: row.try_get("email")?,
        display_name: row.try_get("display_name")?,
        auth_source,
        permissions: Permissions::from_bits_truncate(bits as u64),
        created_at: from_ms(row.try_get("created_at")?),
    })
}

impl UserRepo<'_> {
    pub async fn count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(self.db.pool())
            .await?)
    }

    /// Inserts a user; `DbError::Conflict` on a duplicate username (case-insensitive).
    pub async fn create(&self, u: NewUser) -> Result<User> {
        let now = now_ms();
        let res = sqlx::query(
            "INSERT INTO users (username, email, display_name, password_hash, auth_source, permissions, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&u.username)
        .bind(&u.email)
        .bind(&u.display_name)
        .bind(&u.password_hash)
        .bind(u.auth_source.as_str())
        .bind(u.permissions.bits() as i64)
        .bind(now)
        .bind(now)
        .execute(self.db.pool())
        .await;
        match res {
            Ok(r) => Ok(User {
                id: r.last_insert_rowid(),
                username: u.username,
                email: u.email,
                display_name: u.display_name,
                auth_source: u.auth_source,
                permissions: u.permissions,
                created_at: from_ms(now),
            }),
            Err(e) if is_unique_violation(&e) => Err(DbError::Conflict(format!(
                "username {:?} already exists",
                u.username
            ))),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn get(&self, id: i64) -> Result<Option<User>> {
        let row = sqlx::query(&format!("SELECT {USER_COLS} FROM users u WHERE u.id = ?"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        row.as_ref().map(map_user).transpose()
    }

    /// Looks a user up by username (case-insensitive), returning the password hash too.
    pub async fn find_credentials(&self, username: &str) -> Result<Option<(User, Option<String>)>> {
        let row = sqlx::query(&format!(
            "SELECT {USER_COLS}, u.password_hash FROM users u WHERE u.username = ?"
        ))
        .bind(username)
        .fetch_optional(self.db.pool())
        .await?;
        match row {
            Some(row) => Ok(Some((map_user(&row)?, row.try_get("password_hash")?))),
            None => Ok(None),
        }
    }

    pub async fn list(&self) -> Result<Vec<User>> {
        let rows = sqlx::query(&format!("SELECT {USER_COLS} FROM users u ORDER BY u.id"))
            .fetch_all(self.db.pool())
            .await?;
        rows.iter().map(map_user).collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn new_user(name: &str) -> NewUser {
        NewUser {
            username: name.into(),
            email: Some(format!("{name}@example.com")),
            display_name: None,
            password_hash: Some("hash".into()),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_ADMIN,
        }
    }

    #[tokio::test]
    async fn create_get_list_count() {
        let db = Db::in_memory().await.unwrap();
        assert_eq!(db.users().count().await.unwrap(), 0);
        let u = db.users().create(new_user("alice")).await.unwrap();
        assert!(u.id > 0);
        assert!(u.permissions.is_admin());
        let got = db.users().get(u.id).await.unwrap().unwrap();
        assert_eq!(got, u);
        let mut bob = new_user("bob");
        bob.permissions = Permissions::ROLE_USER;
        bob.auth_source = AuthSource::Jellyfin;
        bob.password_hash = None;
        db.users().create(bob).await.unwrap();
        assert_eq!(db.users().count().await.unwrap(), 2);
        let all = db.users().list().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].auth_source, AuthSource::Jellyfin);
        assert_eq!(all[1].permissions, Permissions::ROLE_USER);
        assert!(db.users().get(999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_username_is_conflict_case_insensitive() {
        let db = Db::in_memory().await.unwrap();
        db.users().create(new_user("Alice")).await.unwrap();
        let err = db.users().create(new_user("alice")).await.unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)), "{err:?}");
    }

    #[tokio::test]
    async fn find_credentials_case_insensitive() {
        let db = Db::in_memory().await.unwrap();
        let u = db.users().create(new_user("Alice")).await.unwrap();
        let (found, hash) = db.users().find_credentials("ALICE").await.unwrap().unwrap();
        assert_eq!(found.id, u.id);
        assert_eq!(found.username, "Alice");
        assert_eq!(hash.as_deref(), Some("hash"));
        assert!(db.users().find_credentials("nobody").await.unwrap().is_none());
    }
}
