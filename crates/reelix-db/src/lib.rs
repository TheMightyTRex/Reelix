//! Reelix persistence: SQLite pool, embedded migrations and typed repositories.
//!
//! Timestamps are stored as INTEGER unix milliseconds and exposed as `DateTime<Utc>`.

mod api_keys;
mod custom_formats;
mod delay_profiles;
mod error;
mod jobs;
mod quality_definitions;
mod quality_profiles;
mod release_profiles;
pub mod secrets;
mod sessions;
mod settings;
mod tasks;
mod users;

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub use api_keys::{ApiKey, ApiKeyRepo};
pub use custom_formats::CustomFormatRepo;
pub use delay_profiles::DelayProfileRepo;
pub use error::DbError;
pub use jobs::{Job, JobRepo, NewJob};
pub use quality_definitions::QualityDefinitionRepo;
pub use quality_profiles::QualityProfileRepo;
pub use release_profiles::ReleaseProfileRepo;
pub use sessions::SessionRepo;
pub use settings::SettingsRepo;
pub use tasks::{ScheduledTask, TaskRepo};
pub use users::{NewUser, UserRepo};

pub type Result<T> = std::result::Result<T, DbError>;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations/sqlite");

/// Current time as unix milliseconds.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Converts unix milliseconds to a UTC timestamp (out-of-range values clamp to the epoch).
pub fn from_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

/// Handle to the database; cheap to clone.
#[derive(Clone, Debug)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Opens/creates the DB (create_if_missing, WAL, foreign_keys=on, busy_timeout 5s,
    /// synchronous=NORMAL, journal_size_limit 64 MiB) and runs migrations.
    pub async fn connect(url: &str) -> Result<Db> {
        let opts = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            .synchronous(SqliteSynchronous::Normal);
        let file = opts.get_filename();
        if let Some(parent) = file.parent()
            && !parent.as_os_str().is_empty()
            && file.as_os_str() != ":memory:"
        {
            std::fs::create_dir_all(parent)?;
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            // Shrink the write-ahead log back to at most 64 MiB after each checkpoint; without a limit it
            // keeps its largest size (after a big import or migration) on disk for good.
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA journal_size_limit = 67108864")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await?;
        Self::migrated(pool).await
    }

    /// Private in-memory DB with migrations applied (single connection kept alive). For tests.
    pub async fn in_memory() -> Result<Db> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(opts)
            .await?;
        Self::migrated(pool).await
    }

    async fn migrated(pool: SqlitePool) -> Result<Db> {
        MIGRATOR.run(&pool).await?;
        Ok(Db { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Runs `PRAGMA optimize` (housekeeping).
    pub async fn optimize(&self) -> Result<()> {
        sqlx::query("PRAGMA optimize").execute(&self.pool).await?;
        Ok(())
    }

    pub fn users(&self) -> UserRepo<'_> {
        UserRepo { db: self }
    }

    pub fn sessions(&self) -> SessionRepo<'_> {
        SessionRepo { db: self }
    }

    pub fn api_keys(&self) -> ApiKeyRepo<'_> {
        ApiKeyRepo { db: self }
    }

    pub fn settings(&self) -> SettingsRepo<'_> {
        SettingsRepo { db: self }
    }

    pub fn jobs(&self) -> JobRepo<'_> {
        JobRepo { db: self }
    }

    pub fn tasks(&self) -> TaskRepo<'_> {
        TaskRepo { db: self }
    }

    pub fn quality_definitions(&self) -> QualityDefinitionRepo<'_> {
        QualityDefinitionRepo { db: self }
    }

    pub fn quality_profiles(&self) -> QualityProfileRepo<'_> {
        QualityProfileRepo { db: self }
    }

    pub fn custom_formats(&self) -> CustomFormatRepo<'_> {
        CustomFormatRepo { db: self }
    }

    pub fn delay_profiles(&self) -> DelayProfileRepo<'_> {
        DelayProfileRepo { db: self }
    }

    pub fn release_profiles(&self) -> ReleaseProfileRepo<'_> {
        ReleaseProfileRepo { db: self }
    }
}

fn decode_err(msg: impl Into<String>) -> DbError {
    DbError::Sqlx(sqlx::Error::Decode(msg.into().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_roundtrip() {
        let now = now_ms();
        assert_eq!(from_ms(now).timestamp_millis(), now);
    }

    #[tokio::test]
    async fn in_memory_persists_across_queries() {
        let db = Db::in_memory().await.unwrap();
        db.ping().await.unwrap();
        db.settings().set("k", &1).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(db.settings().get::<i32>("k").await.unwrap(), Some(1));
    }

    #[tokio::test]
    async fn connect_file_db_creates_dirs_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("reelix.db");
        let url = format!("sqlite://{}", path.display());
        {
            let db = Db::connect(&url).await.unwrap();
            db.settings().set("x", "y").await.unwrap();
            db.optimize().await.unwrap();
            db.pool().close().await;
        }
        let db = Db::connect(&url).await.unwrap();
        assert_eq!(
            db.settings().get::<String>("x").await.unwrap().as_deref(),
            Some("y")
        );
    }
}
