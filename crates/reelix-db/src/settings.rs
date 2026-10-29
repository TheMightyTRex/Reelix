use crate::{Db, Result, now_ms};

/// Key/value settings; values are stored as JSON text.
pub struct SettingsRepo<'a> {
    pub(crate) db: &'a Db,
}

impl SettingsRepo<'_> {
    pub async fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(self.db.pool())
            .await?;
        raw.map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn set<T: serde::Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)?;
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(json)
        .bind(now_ms())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use crate::{Db, DbError};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Cfg {
        a: u32,
        b: Vec<String>,
    }

    #[tokio::test]
    async fn settings_roundtrip_and_overwrite() {
        let db = Db::in_memory().await.unwrap();
        assert!(db.settings().get::<Cfg>("cfg").await.unwrap().is_none());
        let v = Cfg {
            a: 1,
            b: vec!["x".into()],
        };
        db.settings().set("cfg", &v).await.unwrap();
        assert_eq!(db.settings().get::<Cfg>("cfg").await.unwrap(), Some(v));
        db.settings().set("cfg", &Cfg { a: 2, b: vec![] }).await.unwrap();
        assert_eq!(db.settings().get::<Cfg>("cfg").await.unwrap().unwrap().a, 2);
        db.settings().set("s", "str").await.unwrap();
        assert_eq!(
            db.settings().get::<String>("s").await.unwrap().as_deref(),
            Some("str")
        );
        assert!(matches!(
            db.settings().get::<Cfg>("s").await,
            Err(DbError::Json(_))
        ));
    }
}
