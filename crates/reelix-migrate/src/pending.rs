//! Apps saved for a later migration ("pending sources"): the Windows installer finds Radarr,
//! Sonarr, … on the PC and saves their URL and API key with `reelix migrate prepare`, so the
//! admin only has to review the preview in System → Migrate. Nothing is imported unattended.
//!
//! The list lives in the settings table; API keys are encrypted with the server's secret key
//! and never returned. A connection that carries [`SAVED_API_KEY`] for a saved source and URL
//! is completed with the saved key by [`resolve_key`].

use chrono::{DateTime, Utc};
use reelix_api_types::migrate::{MigrateConnection, PendingMigrationSource, SAVED_API_KEY, SourceKind};
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use serde::{Deserialize, Serialize};

use crate::MigrateError;
use crate::source::normalize_url;

/// Settings key holding the list.
pub const SETTINGS_KEY: &str = "migrate.pending_sources";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    source: SourceKind,
    url: String,
    /// Encrypted with the server's [`SecretBox`].
    api_key: String,
    added_at: DateTime<Utc>,
}

fn internal(e: impl std::fmt::Display) -> MigrateError {
    MigrateError::Internal(e.to_string())
}

async fn load(db: &Db) -> Result<Vec<Stored>, MigrateError> {
    Ok(db
        .settings()
        .get::<Vec<Stored>>(SETTINGS_KEY)
        .await
        .map_err(internal)?
        .unwrap_or_default())
}

async fn save(db: &Db, list: &[Stored]) -> Result<(), MigrateError> {
    db.settings().set(SETTINGS_KEY, list).await.map_err(internal)
}

/// Saves (or replaces, for the same app and URL) a source to migrate from later.
pub async fn add(
    db: &Db,
    secrets: &SecretBox,
    source: SourceKind,
    url: &str,
    api_key: &str,
) -> Result<PendingMigrationSource, MigrateError> {
    let url = normalize_url(url)?;
    let api_key = api_key.trim();
    if api_key.is_empty() || api_key == SAVED_API_KEY {
        return Err(MigrateError::Input("the API key is required".into()));
    }
    let stored = Stored {
        source,
        url,
        api_key: secrets.encrypt(api_key).map_err(internal)?,
        added_at: Utc::now(),
    };
    let mut list = load(db).await?;
    list.retain(|s| !(s.source == stored.source && s.url == stored.url));
    list.push(stored.clone());
    save(db, &list).await?;
    Ok(masked(&stored))
}

fn masked(s: &Stored) -> PendingMigrationSource {
    PendingMigrationSource {
        source: s.source,
        url: s.url.clone(),
        api_key: SAVED_API_KEY.into(),
        added_at: s.added_at,
    }
}

/// Saved sources, oldest first, with the keys masked.
pub async fn list(db: &Db) -> Result<Vec<PendingMigrationSource>, MigrateError> {
    Ok(load(db).await?.iter().map(masked).collect())
}

/// Forgets the saved sources of one app (`Some`) or all of them (`None`); returns how many.
pub async fn remove(db: &Db, source: Option<SourceKind>) -> Result<usize, MigrateError> {
    let mut list = load(db).await?;
    let before = list.len();
    list.retain(|s| source.is_some_and(|k| k != s.source));
    if list.len() != before {
        save(db, &list).await?;
    }
    Ok(before - list.len())
}

/// When `c` carries [`SAVED_API_KEY`], replaces it with the saved key of the same app and URL.
/// Returns whether a saved key was used; a masked key without a saved match is an input error.
pub async fn resolve_key(
    db: &Db,
    secrets: &SecretBox,
    c: &mut MigrateConnection,
) -> Result<bool, MigrateError> {
    if c.api_key.trim() != SAVED_API_KEY {
        return Ok(false);
    }
    let url = normalize_url(&c.url)?;
    let saved = load(db)
        .await?
        .into_iter()
        .find(|s| s.source == c.source && s.url == url)
        .ok_or_else(|| {
            MigrateError::Input(format!(
                "no API key is saved for {} at this URL; enter the API key",
                c.source.label()
            ))
        })?;
    c.api_key = secrets
        .decrypt(&saved.api_key)
        .map_err(|e| MigrateError::Internal(format!("the saved API key can't be read: {e}")))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(source: SourceKind, url: &str, key: &str) -> MigrateConnection {
        MigrateConnection {
            source,
            url: url.into(),
            api_key: key.into(),
            path_rewrites: Vec::new(),
            secrets: Default::default(),
        }
    }

    #[tokio::test]
    async fn saved_keys_are_encrypted_masked_and_resolved() {
        let db = Db::in_memory().await.unwrap();
        let sb = SecretBox::from_key([3; 32]);
        let key = "0123456789abcdef0123456789abcdef";
        let added = add(&db, &sb, SourceKind::Radarr, "http://localhost:7878/", key)
            .await
            .unwrap();
        assert_eq!(added.url, "http://localhost:7878");
        assert_eq!(added.api_key, SAVED_API_KEY);
        add(
            &db,
            &sb,
            SourceKind::Sonarr,
            "http://localhost:8989",
            "sonarr-key",
        )
        .await
        .unwrap();
        // Same app and URL again: replaced, not duplicated.
        add(&db, &sb, SourceKind::Radarr, "http://localhost:7878", key)
            .await
            .unwrap();

        let raw: serde_json::Value = db.settings().get(SETTINGS_KEY).await.unwrap().unwrap();
        assert!(!raw.to_string().contains(key), "{raw}");
        let listed = list(&db).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|s| s.api_key == SAVED_API_KEY));

        let mut c = conn(SourceKind::Radarr, "http://localhost:7878", SAVED_API_KEY);
        assert!(resolve_key(&db, &sb, &mut c).await.unwrap());
        assert_eq!(c.api_key, key);
        // A typed key is left alone; a masked key for another URL is refused.
        let mut c = conn(SourceKind::Radarr, "http://other:7878", "typed");
        assert!(!resolve_key(&db, &sb, &mut c).await.unwrap());
        let mut c = conn(SourceKind::Radarr, "http://other:7878", SAVED_API_KEY);
        assert!(matches!(
            resolve_key(&db, &sb, &mut c).await,
            Err(MigrateError::Input(_))
        ));

        assert!(matches!(
            add(&db, &sb, SourceKind::Radarr, "ftp://x", key).await,
            Err(MigrateError::Input(_))
        ));
        assert!(matches!(
            add(&db, &sb, SourceKind::Radarr, "http://x", " ").await,
            Err(MigrateError::Input(_))
        ));

        assert_eq!(remove(&db, Some(SourceKind::Sonarr)).await.unwrap(), 1);
        assert_eq!(list(&db).await.unwrap().len(), 1);
        assert_eq!(remove(&db, None).await.unwrap(), 1);
        assert!(list(&db).await.unwrap().is_empty());
    }
}
