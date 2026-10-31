//! System maintenance: backups and restore, the log viewer, update checks and server settings.
//!
//! [`MaintenanceService`] implements the API's [`MaintenanceBackend`], the `backup` and
//! `update_check` scheduled tasks, and health checks for staged restores and new releases.

pub mod backup;
pub mod logs;
pub mod server_settings;
pub mod updates;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use reelix_api::{ApiError, ApiResult, HealthSource, LogsQuery, MaintenanceBackend};
use reelix_api_types::system::{
    BackupDto, BackupKind, BackupSettings, BackupsOverview, InstallKind, LogFileDto, LogsDto,
    PendingRestoreDto, ReleaseDto, ServerSettingsDto, ServerSettingsValues, UpdateSettings, UpdateStatusDto,
};
use reelix_api_types::{HealthCheck, HealthLevel};
use reelix_db::Db;
use reelix_jobs::{JobContext, JobHandler, JobSystemBuilder, TaskDef};
use serde::{Deserialize, Serialize};

use crate::config::Config;

const BACKUP_SETTINGS_KEY: &str = "system.backup";
const UPDATE_SETTINGS_KEY: &str = "system.updates";
/// Job kind of the scheduled backup task.
pub const BACKUP_TASK: &str = "backup";
/// Job kind of the update check task.
pub const UPDATE_CHECK_TASK: &str = "update_check";

/// Where maintenance reads and writes files.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub log_dir: PathBuf,
    /// SQLite database file; `None` for other databases (backups are then unavailable).
    pub db_file: Option<PathBuf>,
    pub config_file: PathBuf,
    pub secret_key_file: PathBuf,
    /// False when the key comes from `REELIX_SECRET_KEY`: backups then leave the key file alone.
    pub backup_secret_key: bool,
}

impl Paths {
    pub fn from_config(cfg: &Config) -> Self {
        let db_file = sqlite_file(&cfg.database_url);
        Self {
            data_dir: cfg.data_dir.clone(),
            backup_dir: cfg.backup_dir.clone(),
            log_dir: cfg.log_dir.clone(),
            db_file,
            config_file: cfg
                .config_file
                .clone()
                .unwrap_or_else(|| cfg.default_config_path()),
            secret_key_file: cfg.secret_key_file.clone(),
            backup_secret_key: std::env::var_os("REELIX_SECRET_KEY").is_none_or(|v| v.is_empty()),
        }
    }
}

/// The database file of a `sqlite:` URL.
fn sqlite_file(url: &str) -> Option<PathBuf> {
    use std::str::FromStr;
    if !url.starts_with("sqlite:") || url.contains(":memory:") || url.contains("mode=memory") {
        return None;
    }
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str(url).ok()?;
    let file = opts.get_filename();
    (file.as_os_str() != ":memory:" && !file.as_os_str().is_empty()).then(|| file.to_path_buf())
}

/// Applies a restore staged before the last shutdown. Call before the database is opened.
pub async fn apply_pending_restore(cfg: &Config) {
    match backup::apply_pending_restore(&Paths::from_config(cfg), &cfg.database_url).await {
        Ok(_) => {}
        Err(e) => tracing::error!(error = format!("{e:#}"), "could not restore the backup"),
    }
}

fn install_kind() -> InstallKind {
    if std::path::Path::new("/.dockerenv").exists() || std::env::var_os("REELIX_DOCKER").is_some() {
        InstallKind::Docker
    } else if cfg!(windows) {
        InstallKind::Windows
    } else {
        InstallKind::Other
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredUpdateSettings {
    automatic: bool,
}

impl Default for StoredUpdateSettings {
    fn default() -> Self {
        Self { automatic: true }
    }
}

#[derive(Default)]
struct UpdateCache {
    releases: Vec<ReleaseDto>,
    checked_at: Option<DateTime<Utc>>,
    error: Option<String>,
    automatic: bool,
}

pub struct MaintenanceService {
    db: Db,
    http: reqwest::Client,
    paths: Paths,
    config: Config,
    github_api: String,
    /// One backup or restore operation at a time.
    backup_lock: tokio::sync::Mutex<()>,
    updates: Mutex<UpdateCache>,
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(e)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(internal)
}

impl MaintenanceService {
    pub async fn new(db: Db, http: reqwest::Client, config: &Config) -> anyhow::Result<Arc<Self>> {
        let automatic = db
            .settings()
            .get::<StoredUpdateSettings>(UPDATE_SETTINGS_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
            .automatic;
        Ok(Arc::new(Self {
            db,
            http,
            paths: Paths::from_config(config),
            config: config.clone(),
            github_api: updates::GITHUB_API.into(),
            backup_lock: tokio::sync::Mutex::new(()),
            updates: Mutex::new(UpdateCache {
                automatic,
                ..UpdateCache::default()
            }),
        }))
    }

    pub fn register(builder: JobSystemBuilder, svc: &Arc<Self>) -> JobSystemBuilder {
        builder
            .handler(BackupJob(svc.clone()))
            .handler(UpdateCheckJob(svc.clone()))
            .task(TaskDef {
                kind: BACKUP_TASK,
                name: "Scheduled backup",
                // Runs daily and backs up when the configured interval has passed.
                interval: Duration::from_secs(24 * 3600),
                run_on_startup: false,
            })
            .task(TaskDef {
                kind: UPDATE_CHECK_TASK,
                name: "Check for updates",
                interval: Duration::from_secs(12 * 3600),
                run_on_startup: true,
            })
    }

    async fn backup_settings(&self) -> BackupSettings {
        self.db
            .settings()
            .get::<BackupSettings>(BACKUP_SETTINGS_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    async fn make_backup(&self, kind: BackupKind) -> anyhow::Result<BackupDto> {
        if self.paths.db_file.is_none() {
            anyhow::bail!("backups are only supported for SQLite databases");
        }
        let _guard = self.backup_lock.lock().await;
        std::fs::create_dir_all(&self.paths.backup_dir)?;
        let snapshot = self
            .paths
            .backup_dir
            .join(format!(".snapshot-{}.db", rand::random::<u64>()));
        // VACUUM INTO writes a consistent, compacted copy while the server keeps running.
        let vacuum = sqlx::query("VACUUM INTO ?")
            .bind(snapshot.to_string_lossy().into_owned())
            .execute(self.db.pool())
            .await;
        let result = match vacuum {
            Ok(_) => {
                let paths = self.paths.clone();
                let snap = snapshot.clone();
                tokio::task::spawn_blocking(move || backup::write_archive(&paths, &snap, kind, Utc::now()))
                    .await?
            }
            Err(e) => Err(anyhow::Error::new(e).context("snapshotting the database")),
        };
        let _ = std::fs::remove_file(&snapshot);
        let path = result?;
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Ok(BackupDto {
            name,
            kind,
            size_bytes,
            created_at: Utc::now(),
            version: Some(reelix_core::VERSION.into()),
        })
    }

    /// The scheduled task: back up when the interval has passed since the last scheduled backup.
    async fn scheduled_backup(&self) -> anyhow::Result<()> {
        let settings = self.backup_settings().await;
        if settings.interval_days == 0 {
            tracing::debug!("scheduled backups are turned off");
            return Ok(());
        }
        let paths = self.paths.clone();
        let existing = tokio::task::spawn_blocking(move || backup::list(&paths)).await??;
        let last = existing
            .iter()
            .filter(|b| b.kind == BackupKind::Scheduled)
            .map(|b| b.created_at)
            .max();
        // An hour of slack so a daily task doesn't skip a day because of scheduling jitter.
        let due = chrono::Duration::days(i64::from(settings.interval_days)) - chrono::Duration::hours(1);
        if last.is_some_and(|l| Utc::now() - l < due) {
            return Ok(());
        }
        let made = self.make_backup(BackupKind::Scheduled).await?;
        let paths = self.paths.clone();
        let keep = settings.retention as usize;
        let pruned = tokio::task::spawn_blocking(move || backup::prune_scheduled(&paths, keep)).await??;
        tracing::info!(name = %made.name, pruned, "scheduled backup created");
        Ok(())
    }

    async fn check_updates(&self) {
        let result = updates::fetch(&self.http, &self.github_api, reelix_core::VERSION).await;
        let mut cache = self.updates.lock().unwrap_or_else(|e| e.into_inner());
        cache.checked_at = Some(Utc::now());
        match result {
            Ok(releases) => {
                if let Some(latest) = releases.first().filter(|r| r.newer) {
                    tracing::info!(version = %latest.version, "a Reelix update is available");
                }
                cache.releases = releases;
                cache.error = None;
            }
            Err(e) => {
                tracing::warn!(error = %e, "update check failed");
                cache.error = Some(e);
            }
        }
    }

    fn update_snapshot(&self) -> UpdateStatusDto {
        let cache = self.updates.lock().unwrap_or_else(|e| e.into_inner());
        let latest = cache.releases.first().cloned();
        UpdateStatusDto {
            current_version: reelix_core::VERSION.into(),
            repository: updates::REPOSITORY.into(),
            install: install_kind(),
            update_available: latest.as_ref().is_some_and(|r| r.newer),
            latest,
            releases: cache.releases.clone(),
            checked_at: cache.checked_at,
            error: cache.error.clone(),
            automatic: cache.automatic,
        }
    }
}

impl MaintenanceBackend for MaintenanceService {
    fn backups(&self) -> BoxFuture<'_, ApiResult<BackupsOverview>> {
        Box::pin(async move {
            let paths = self.paths.clone();
            let (backups, pending) =
                blocking(move || (backup::list(&paths), backup::pending(&paths))).await?;
            Ok(BackupsOverview {
                backups: backups.map_err(internal)?,
                settings: self.backup_settings().await,
                backup_dir: self.paths.backup_dir.display().to_string(),
                pending_restore: pending,
                includes_secret_key: self.paths.backup_secret_key,
            })
        })
    }

    fn create_backup(&self, kind: BackupKind) -> BoxFuture<'_, ApiResult<BackupDto>> {
        Box::pin(async move {
            self.make_backup(kind).await.map_err(|e| {
                tracing::error!(error = format!("{e:#}"), "backup failed");
                ApiError::internal(format!("{e:#}"))
            })
        })
    }

    fn backup_path(&self, name: &str) -> ApiResult<PathBuf> {
        let path = self.paths.backup_dir.join(name);
        if backup::is_safe_name(name) && name.ends_with(".zip") && path.is_file() {
            Ok(path)
        } else {
            Err(ApiError::not_found(format!("backup '{name}' does not exist")))
        }
    }

    fn delete_backup(&self, name: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            let path = self.backup_path(&name)?;
            tokio::fs::remove_file(path).await.map_err(internal)
        })
    }

    fn set_backup_settings(&self, settings: BackupSettings) -> BoxFuture<'_, ApiResult<BackupSettings>> {
        Box::pin(async move {
            if settings.interval_days > 365 {
                return Err(ApiError::validation(
                    "the backup interval can be at most 365 days",
                ));
            }
            if !(1..=100).contains(&settings.retention) {
                return Err(ApiError::validation("keep between 1 and 100 scheduled backups"));
            }
            self.db.settings().set(BACKUP_SETTINGS_KEY, &settings).await?;
            Ok(settings)
        })
    }

    fn stage_restore(&self, name: String) -> BoxFuture<'_, ApiResult<PendingRestoreDto>> {
        Box::pin(async move {
            let path = self.backup_path(&name)?;
            let _guard = self.backup_lock.lock().await;
            let paths = self.paths.clone();
            blocking(move || backup::stage(&paths, &path, &name, Utc::now()))
                .await?
                .map_err(|e| ApiError::validation(format!("{e:#}")))
        })
    }

    fn stage_restore_upload(&self, archive: Bytes) -> BoxFuture<'_, ApiResult<PendingRestoreDto>> {
        Box::pin(async move {
            let _guard = self.backup_lock.lock().await;
            let paths = self.paths.clone();
            blocking(move || backup::stage_bytes(&paths, &archive, Utc::now()))
                .await?
                .map_err(|e| ApiError::validation(format!("{e:#}")))
        })
    }

    fn cancel_restore(&self) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            let _guard = self.backup_lock.lock().await;
            let paths = self.paths.clone();
            blocking(move || backup::cancel(&paths)).await?.map_err(internal)
        })
    }

    fn log_files(&self) -> BoxFuture<'_, ApiResult<Vec<LogFileDto>>> {
        Box::pin(async move {
            let dir = self.paths.log_dir.clone();
            blocking(move || logs::files(&dir)).await?.map_err(internal)
        })
    }

    fn log_path(&self, name: &str) -> ApiResult<PathBuf> {
        logs::path(&self.paths.log_dir, name)
            .ok_or_else(|| ApiError::not_found(format!("log file '{name}' does not exist")))
    }

    fn logs(&self, query: LogsQuery) -> BoxFuture<'_, ApiResult<LogsDto>> {
        Box::pin(async move {
            let dir = self.paths.log_dir.clone();
            let filter = logs::Filter::new(query.level.as_deref(), query.search.as_deref(), query.limit);
            match blocking(move || logs::logs(&dir, query.file.as_deref(), &filter)).await? {
                Ok(l) => Ok(l),
                Err(logs::ReadError::NotFound) => Err(ApiError::not_found("unknown log file")),
                Err(e) => Err(internal(e)),
            }
        })
    }

    fn update_status(&self, force: bool) -> BoxFuture<'_, ApiResult<UpdateStatusDto>> {
        Box::pin(async move {
            let never = self
                .updates
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .checked_at
                .is_none();
            if force || never {
                self.check_updates().await;
            }
            Ok(self.update_snapshot())
        })
    }

    fn set_update_settings(&self, settings: UpdateSettings) -> BoxFuture<'_, ApiResult<UpdateStatusDto>> {
        Box::pin(async move {
            self.db
                .settings()
                .set(
                    UPDATE_SETTINGS_KEY,
                    &StoredUpdateSettings {
                        automatic: settings.automatic,
                    },
                )
                .await?;
            self.updates.lock().unwrap_or_else(|e| e.into_inner()).automatic = settings.automatic;
            Ok(self.update_snapshot())
        })
    }

    fn server_settings(&self) -> BoxFuture<'_, ApiResult<ServerSettingsDto>> {
        Box::pin(async move {
            let path = self.paths.config_file.clone();
            let saved = blocking(move || server_settings::saved(&path))
                .await?
                .map_err(|e| ApiError::validation(format!("config.toml could not be read: {e}")))?;
            Ok(self.settings_dto(saved))
        })
    }

    fn set_server_settings(
        &self,
        values: ServerSettingsValues,
    ) -> BoxFuture<'_, ApiResult<ServerSettingsDto>> {
        Box::pin(async move {
            let values = server_settings::validate(&values).map_err(ApiError::validation)?;
            let path = self.paths.config_file.clone();
            let v = values.clone();
            blocking(move || server_settings::save(&path, &v))
                .await?
                .map_err(|e| ApiError::internal(format!("could not write config.toml: {e}")))?;
            Ok(self.settings_dto(values))
        })
    }
}

impl MaintenanceService {
    fn settings_dto(&self, saved: ServerSettingsValues) -> ServerSettingsDto {
        let running = server_settings::running(&self.config);
        let overridden = server_settings::overridden(&self.config);
        let forced = |key: &str| overridden.iter().any(|o| o.starts_with(&format!("{key} ")));
        let restart_required = (saved.bind != running.bind && !forced("bind"))
            || (saved.port != running.port && !forced("port"))
            || (saved.url_base != running.url_base && !forced("url_base"))
            || (saved.log_level != running.log_level && !forced("log_level"))
            || (saved.log_format != running.log_format && !forced("log_format"));
        ServerSettingsDto {
            saved,
            running,
            config_file: self.paths.config_file.display().to_string(),
            overridden,
            restart_required,
        }
    }
}

impl HealthSource for MaintenanceService {
    fn checks(&self) -> Vec<HealthCheck> {
        let mut out = Vec::new();
        if let Some(p) = backup::pending(&self.paths) {
            out.push(HealthCheck {
                source: "backup".into(),
                level: HealthLevel::Warning,
                message: format!(
                    "A restore from {} is staged; restart Reelix to apply it",
                    p.source
                ),
            });
        }
        let status = self.update_snapshot();
        if status.automatic
            && let Some(latest) = status.latest.filter(|r| r.newer)
        {
            out.push(HealthCheck {
                source: "updates".into(),
                level: HealthLevel::Warning,
                message: format!(
                    "Reelix {} is available (you're running {}); see System → Updates",
                    latest.version, status.current_version
                ),
            });
        }
        out
    }
}

/// kind "backup": a scheduled backup when the configured interval has passed.
pub struct BackupJob(pub Arc<MaintenanceService>);

#[async_trait::async_trait]
impl JobHandler for BackupJob {
    fn kind(&self) -> &'static str {
        BACKUP_TASK
    }

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        self.0.scheduled_backup().await
    }
}

/// kind "update_check": refreshes the cached release list (skipped when automatic checks are off).
pub struct UpdateCheckJob(pub Arc<MaintenanceService>);

#[async_trait::async_trait]
impl JobHandler for UpdateCheckJob {
    fn kind(&self) -> &'static str {
        UPDATE_CHECK_TASK
    }

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        // "Check now" on the Updates page checks regardless of this setting.
        let automatic = self.0.updates.lock().unwrap_or_else(|e| e.into_inner()).automatic;
        if automatic {
            self.0.check_updates().await;
        } else {
            tracing::debug!("automatic update checks are turned off");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_file_from_url() {
        assert_eq!(
            sqlite_file("sqlite:///config/reelix.db"),
            Some(PathBuf::from("/config/reelix.db"))
        );
        assert_eq!(
            sqlite_file("sqlite://./data/reelix.db"),
            Some(PathBuf::from("./data/reelix.db"))
        );
        assert_eq!(sqlite_file("sqlite::memory:"), None);
        assert_eq!(sqlite_file("postgres://x/y"), None);
    }
}
