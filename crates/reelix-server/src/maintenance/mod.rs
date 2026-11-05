//! System maintenance: backups and restore, the log viewer, update checks and server settings.
//!
//! [`MaintenanceService`] implements the API's [`MaintenanceBackend`], the `backup` and
//! `update_check` scheduled tasks, and health checks for staged restores and new releases.

pub mod backup;
pub mod logs;
pub mod self_update;
pub mod server_settings;
pub mod updates;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use reelix_api::{ApiError, ApiResult, HealthSource, LogsQuery, MaintenanceBackend, UploadStream};
use reelix_api_types::system::{
    BackupDto, BackupKind, BackupSettings, BackupsOverview, InstallKind, LogFileDto, LogsDto,
    PendingRestoreDto, ReleaseDto, SelfUpdateDto, SelfUpdateState, ServerSettingsDto, ServerSettingsValues,
    UpdateSettings, UpdateStatusDto,
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

/// Snapshots the database into a new backup archive and returns its path.
pub async fn snapshot_backup(db: &Db, paths: &Paths, kind: BackupKind) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(&paths.backup_dir)?;
    let snapshot = paths
        .backup_dir
        .join(format!(".snapshot-{}.db", rand::random::<u64>()));
    // VACUUM INTO writes a consistent, compacted copy while the server keeps running.
    let vacuum = sqlx::query("VACUUM INTO ?")
        .bind(snapshot.to_string_lossy().into_owned())
        .execute(db.pool())
        .await;
    let result = match vacuum {
        Ok(_) => {
            let paths = paths.clone();
            let snap = snapshot.clone();
            tokio::task::spawn_blocking(move || backup::write_archive(&paths, &snap, kind, Utc::now()))
                .await?
        }
        Err(e) => Err(anyhow::Error::new(e).context("snapshotting the database")),
    };
    let _ = std::fs::remove_file(&snapshot);
    result
}

fn install_kind() -> InstallKind {
    if self_update::in_docker() {
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
    #[serde(default)]
    auto_install: bool,
}

impl Default for StoredUpdateSettings {
    fn default() -> Self {
        Self {
            automatic: true,
            auto_install: false,
        }
    }
}

#[derive(Default)]
struct UpdateCache {
    releases: Vec<ReleaseDto>,
    checked_at: Option<DateTime<Utc>>,
    error: Option<String>,
    automatic: bool,
    auto_install: bool,
}

/// Progress of the current (or last) self-update.
#[derive(Default)]
struct InstallProgress {
    state: SelfUpdateState,
    version: Option<String>,
    error: Option<String>,
    backup: Option<String>,
}

/// Where updates come from and how they are installed; tests point these at a mock server.
pub struct UpdateOptions {
    pub github_api: String,
    pub host_policy: self_update::HostPolicy,
    /// `None` when the binary's location is unknown (self-update is then unsupported).
    pub install: Option<self_update::Install>,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            github_api: updates::GITHUB_API.into(),
            host_policy: self_update::HostPolicy::default(),
            install: self_update::current().cloned(),
        }
    }
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
    /// Downloads release archives; only follows redirects to GitHub.
    download_http: reqwest::Client,
    host_policy: self_update::HostPolicy,
    install: Option<self_update::Install>,
    progress: Mutex<InstallProgress>,
    /// For spawning the background install from `&self`.
    me: Weak<MaintenanceService>,
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(e)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(internal)
}

/// A file removed when dropped (also when the request is cancelled mid-upload).
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes the streamed upload to `to`; returns the number of bytes written.
async fn save_upload(archive: &mut UploadStream, to: &Path) -> std::io::Result<u64> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::File::create(to).await?;
    let mut written = 0u64;
    while let Some(chunk) = archive.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    file.sync_all().await?;
    Ok(written)
}

impl MaintenanceService {
    pub async fn new(db: Db, http: reqwest::Client, config: &Config) -> anyhow::Result<Arc<Self>> {
        Self::with_options(db, http, config, UpdateOptions::default()).await
    }

    pub async fn with_options(
        db: Db,
        http: reqwest::Client,
        config: &Config,
        options: UpdateOptions,
    ) -> anyhow::Result<Arc<Self>> {
        let stored = db
            .settings()
            .get::<StoredUpdateSettings>(UPDATE_SETTINGS_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let download_http = self_update::download_client(options.host_policy)?;
        Ok(Arc::new_cyclic(|me| Self {
            db,
            http,
            paths: Paths::from_config(config),
            config: config.clone(),
            github_api: options.github_api,
            backup_lock: tokio::sync::Mutex::new(()),
            updates: Mutex::new(UpdateCache {
                automatic: stored.automatic,
                auto_install: stored.auto_install,
                ..UpdateCache::default()
            }),
            download_http,
            host_policy: options.host_policy,
            install: options.install,
            progress: Mutex::new(InstallProgress::default()),
            me: me.clone(),
        }))
    }

    pub fn register(builder: JobSystemBuilder, svc: &Arc<Self>) -> JobSystemBuilder {
        builder
            .long_running_handler(BackupJob(svc.clone()))
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
        self.backup_locked(kind).await
    }

    /// [`Self::make_backup`] for a caller already holding `backup_lock`.
    async fn backup_locked(&self, kind: BackupKind) -> anyhow::Result<BackupDto> {
        let path = snapshot_backup(&self.db, &self.paths, kind).await?;
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

    fn self_update_snapshot(&self, auto_install: bool) -> SelfUpdateDto {
        let progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        let (kind, reason) = match &self.install {
            Some(i) => (i.kind.as_str(), i.unsupported.clone()),
            None => (
                self_update::Kind::Source.as_str(),
                Some("the location of the Reelix binary is unknown".into()),
            ),
        };
        SelfUpdateDto {
            supported: reason.is_none(),
            reason,
            kind: kind.into(),
            state: progress.state,
            version: progress.version.clone(),
            error: progress.error.clone(),
            backup: progress.backup.clone(),
            auto_install,
        }
    }

    fn set_install_state(&self, state: SelfUpdateState) {
        self.progress.lock().unwrap_or_else(|e| e.into_inner()).state = state;
    }

    /// Validates `version` and starts installing it in the background.
    fn start_install(&self, version: &str) -> ApiResult<()> {
        let install = match &self.install {
            Some(i) if i.supported() => i.clone(),
            Some(i) => {
                return Err(ApiError::Conflict(format!(
                    "Reelix can't update itself: {}",
                    i.unsupported.as_deref().unwrap_or_default()
                )));
            }
            None => {
                return Err(ApiError::Conflict(
                    "Reelix can't update itself: the location of the binary is unknown".into(),
                ));
            }
        };
        let version = version.trim().trim_start_matches(['v', 'V']);
        let release = self
            .updates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .releases
            .iter()
            .find(|r| r.version == version)
            .cloned()
            .ok_or_else(|| {
                ApiError::validation(format!(
                    "{version} is not a known release; check for updates first"
                ))
            })?;
        // No downgrades or reinstalls: only versions newer than the running one.
        if !updates::is_newer(&release.version, reelix_core::VERSION) {
            return Err(ApiError::Conflict(format!(
                "{} is not newer than the running version {}",
                release.version,
                reelix_core::VERSION
            )));
        }
        let assets = self_update::select_assets_here(&release).map_err(ApiError::validation)?;
        let me = self
            .me
            .upgrade()
            .ok_or_else(|| internal("the maintenance service is gone"))?;
        {
            let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            if progress.state.busy() {
                return Err(ApiError::Conflict(format!(
                    "the update to {} is already being installed",
                    progress.version.as_deref().unwrap_or("a new version")
                )));
            }
            *progress = InstallProgress {
                state: SelfUpdateState::Downloading,
                version: Some(release.version.clone()),
                ..InstallProgress::default()
            };
        }
        tokio::spawn(async move {
            let version = release.version.clone();
            match me.run_install(&install, &release, &assets).await {
                Ok(()) => {
                    tracing::warn!(%version, "update installed; restarting Reelix");
                    me.set_install_state(SelfUpdateState::Restarting);
                    crate::lifecycle::request_restart(install.binary());
                }
                Err(e) => {
                    let error = format!("{e:#}");
                    tracing::error!(%version, %error, "installing the update failed");
                    let mut progress = me.progress.lock().unwrap_or_else(|e| e.into_inner());
                    progress.state = SelfUpdateState::Failed;
                    progress.error = Some(error);
                }
            }
        });
        Ok(())
    }

    /// Download, verify, back up and swap. The new files are in place when this returns `Ok`.
    async fn run_install(
        &self,
        install: &self_update::Install,
        release: &ReleaseDto,
        assets: &self_update::Assets,
    ) -> anyhow::Result<()> {
        let staging = self_update::staging_dir(&install.dir, &release.version);
        let result = self.stage_and_swap(install, release, assets, &staging).await;
        let cleanup = staging.clone();
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(cleanup)).await;
        result
    }

    async fn stage_and_swap(
        &self,
        install: &self_update::Install,
        release: &ReleaseDto,
        assets: &self_update::Assets,
        staging: &Path,
    ) -> anyhow::Result<()> {
        use anyhow::Context;

        let _ = std::fs::remove_dir_all(staging);
        std::fs::create_dir_all(staging).with_context(|| {
            format!(
                "Reelix can't write to its install folder {}",
                install.dir.display()
            )
        })?;
        tracing::info!(version = %release.version, asset = %assets.archive.name, "downloading update");
        let sums = self_update::fetch_text(
            &self.download_http,
            &assets.sums.url,
            self.host_policy,
            self_update::MAX_SUMS,
        )
        .await?;
        let archive = staging.join(&assets.archive.name);
        let sha256 = self_update::download(
            &self.download_http,
            &assets.archive.url,
            self.host_policy,
            self_update::MAX_ARCHIVE,
            &archive,
        )
        .await?;

        self.set_install_state(SelfUpdateState::Verifying);
        self_update::verify_checksum(&sums, &assets.archive.name, &sha256).map_err(anyhow::Error::msg)?;
        let (format, top, dest) = (assets.format, assets.top.clone(), staging.join("unpacked"));
        let staged = tokio::task::spawn_blocking(move || self_update::extract(&archive, format, &dest, &top))
            .await??;

        // Held until the files are swapped, so no restore is staged halfway through.
        self.set_install_state(SelfUpdateState::BackingUp);
        let _guard = self.backup_lock.lock().await;
        if self.paths.db_file.is_some() {
            let backup = self
                .backup_locked(BackupKind::Manual)
                .await
                .context("backing up before the update")?;
            tracing::info!(name = %backup.name, "backup created before the update; restore it if the new version misbehaves");
            self.progress.lock().unwrap_or_else(|e| e.into_inner()).backup = Some(backup.name);
        } else {
            tracing::warn!("no backup before the update: backups are only supported for SQLite databases");
        }

        self.set_install_state(SelfUpdateState::Installing);
        let dir = install.dir.clone();
        tokio::task::spawn_blocking(move || self_update::swap(&dir, &staged)).await??;
        Ok(())
    }

    /// After a scheduled check: installs the newest non-pre-release update when auto-install is on.
    fn auto_install(&self) {
        let candidate = {
            let cache = self.updates.lock().unwrap_or_else(|e| e.into_inner());
            if !cache.auto_install || cache.error.is_some() {
                return;
            }
            cache
                .releases
                .iter()
                .find(|r| r.newer && !r.prerelease)
                .map(|r| r.version.clone())
        };
        let Some(version) = candidate else {
            return;
        };
        if !self.install.as_ref().is_some_and(|i| i.supported())
            || self
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .state
                .busy()
        {
            return;
        }
        tracing::warn!(%version, "installing update automatically");
        if let Err(e) = self.start_install(&version) {
            tracing::warn!(%version, error = %e, "automatic update not started");
        }
    }

    fn update_snapshot(&self) -> UpdateStatusDto {
        let cache = self.updates.lock().unwrap_or_else(|e| e.into_inner());
        let latest = cache.releases.first().cloned();
        UpdateStatusDto {
            self_update: self.self_update_snapshot(cache.auto_install),
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

    fn stage_restore_upload(&self, mut archive: UploadStream) -> BoxFuture<'_, ApiResult<PendingRestoreDto>> {
        Box::pin(async move {
            let paths = self.paths.clone();
            let upload = TempFile(
                blocking(move || backup::upload_path(&paths))
                    .await?
                    .map_err(internal)?,
            );
            match save_upload(&mut archive, &upload.0).await {
                Ok(0) => return Err(ApiError::validation("upload a backup .zip file")),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
                    return Err(reelix_api::upload_too_large());
                }
                Err(e) => return Err(ApiError::validation(format!("the upload failed: {e}"))),
            }
            let _guard = self.backup_lock.lock().await;
            let (paths, path) = (self.paths.clone(), upload.0.clone());
            // On success the file is moved into place, so dropping `upload` removes nothing.
            blocking(move || backup::stage_upload(&paths, &path, Utc::now()))
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
                        auto_install: settings.auto_install,
                    },
                )
                .await?;
            {
                let mut cache = self.updates.lock().unwrap_or_else(|e| e.into_inner());
                cache.automatic = settings.automatic;
                cache.auto_install = settings.auto_install;
            }
            Ok(self.update_snapshot())
        })
    }

    fn install_update(&self, version: String) -> BoxFuture<'_, ApiResult<UpdateStatusDto>> {
        Box::pin(async move {
            self.start_install(&version)?;
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

/// kind "update_check": refreshes the cached release list (skipped when automatic checks are off)
/// and, with auto-install on, installs a new release.
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
            self.0.auto_install();
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

#[cfg(test)]
mod self_update_tests {
    use std::path::Path;
    use std::time::Duration;

    use reelix_api_types::system::SelfUpdateState;
    use sha2::{Digest, Sha256};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::self_update::{self, BINARY, INSTALL_MARKER};
    use super::*;
    use crate::config::CliOverrides;

    const OS: &str = if cfg!(windows) { "windows" } else { "linux" };

    /// A release archive for this platform holding version `version`.
    fn archive(dir: &Path, version: &str) -> (String, Vec<u8>) {
        let top = format!("reelix-{version}-{OS}-{}", std::env::consts::ARCH);
        let name = self_update::archive_name(version, OS, std::env::consts::ARCH);
        let files: Vec<(String, Vec<u8>)> = vec![
            (
                format!("{top}/{BINARY}"),
                format!("binary {version}").into_bytes(),
            ),
            (format!("{top}/web/index.html"), version.as_bytes().to_vec()),
            (format!("{top}/docs/guide.md"), b"docs".to_vec()),
            (format!("{top}/README.txt"), b"readme".to_vec()),
            (format!("{top}/{}", self_update::START_SCRIPT), b"start".to_vec()),
            (
                format!("{top}/{INSTALL_MARKER}"),
                br#"{"kind": "portable"}"#.to_vec(),
            ),
        ];
        let path = dir.join(&name);
        if cfg!(windows) {
            let entries: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
            self_update::tests::zip_with(&path, &entries);
        } else {
            let entries: Vec<(&str, &[u8], u32)> = files
                .iter()
                .map(|(n, d)| (n.as_str(), d.as_slice(), 0o644))
                .collect();
            self_update::tests::tar_with(&path, &entries);
        }
        (name, std::fs::read(path).unwrap())
    }

    async fn wait_until_done(svc: &MaintenanceService) -> SelfUpdateDto {
        for _ in 0..300 {
            let s = svc.update_snapshot().self_update;
            if matches!(s.state, SelfUpdateState::Restarting | SelfUpdateState::Failed) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the install did not finish");
    }

    #[tokio::test]
    async fn installs_a_verified_release_and_requests_a_restart() {
        // No release builds exist for this OS (e.g. macOS), so self-update is unsupported here.
        if self_update::platform().is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("install");
        std::fs::create_dir_all(dir.join("web")).unwrap();
        std::fs::write(dir.join(BINARY), "binary old").unwrap();
        std::fs::write(dir.join("web/index.html"), "old").unwrap();
        std::fs::write(dir.join(self_update::START_SCRIPT), "old start").unwrap();
        std::fs::write(dir.join(INSTALL_MARKER), r#"{"kind": "portable"}"#).unwrap();

        let server = MockServer::start().await;
        let base = server.uri();
        let (good_name, good) = archive(tmp.path(), "99.0.0");
        let (bad_name, bad) = archive(tmp.path(), "98.0.0");
        let sha = |b: &[u8]| hex::encode(Sha256::digest(b));
        // 98.0.0's checksum is wrong on purpose.
        let sums = format!(
            "{}  {good_name}\n{}  {bad_name}\n",
            sha(&good),
            sha(b"something else")
        );
        let asset = |name: &str| serde_json::json!({"name": name, "browser_download_url": format!("{base}/dl/{name}"), "size": 1});
        Mock::given(method("GET"))
            .and(path(format!("/repos/{}/releases", updates::REPOSITORY)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"tag_name": "v99.0.0", "html_url": "https://x/99", "assets": [asset(&good_name), asset("SHA256SUMS")]},
                {"tag_name": "v98.0.0", "html_url": "https://x/98", "assets": [asset(&bad_name), asset("SHA256SUMS")]},
                {"tag_name": "v97.0.0", "html_url": "https://x/97", "assets": [asset("SHA256SUMS")]},
                {"tag_name": "v0.0.1", "html_url": "https://x/1", "assets": []}
            ])))
            .mount(&server)
            .await;
        for (name, body) in [
            (good_name.clone(), good),
            (bad_name, bad),
            ("SHA256SUMS".into(), sums.into_bytes()),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/dl/{name}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                .mount(&server)
                .await;
        }

        let config = Config::load(&CliOverrides {
            data_dir: Some(dir.join("data")),
            ..CliOverrides::default()
        })
        .unwrap();
        let db = Db::connect(&config.database_url).await.unwrap();
        let svc = MaintenanceService::with_options(
            db,
            reqwest::Client::builder().user_agent("test").build().unwrap(),
            &config,
            UpdateOptions {
                github_api: base.clone(),
                host_policy: self_update::HostPolicy {
                    allow_loopback_http: true,
                },
                install: Some(self_update::detect(&dir, false)),
            },
        )
        .await
        .unwrap();
        let status = svc.update_status(true).await.unwrap();
        assert!(status.self_update.supported, "{:?}", status.self_update);
        assert_eq!(status.self_update.kind, "portable");

        let err = svc.install_update("42.0.0".into()).await.unwrap_err();
        assert_eq!(
            err.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "unknown version"
        );
        let err = svc.install_update("0.0.1".into()).await.unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::CONFLICT, "no downgrades");
        let err = svc.install_update("97.0.0".into()).await.unwrap_err();
        assert_eq!(
            err.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "no archive for this platform"
        );

        // A checksum mismatch fails before anything is replaced.
        svc.install_update("98.0.0".into()).await.unwrap();
        let failed = wait_until_done(&svc).await;
        assert_eq!(failed.state, SelfUpdateState::Failed);
        assert!(
            failed.error.as_deref().unwrap().contains("doesn't match"),
            "{failed:?}"
        );
        assert_eq!(std::fs::read_to_string(dir.join(BINARY)).unwrap(), "binary old");
        assert!(!self_update::staging_dir(&dir, "98.0.0").exists());
        assert!(!crate::lifecycle::RESTART_REQUESTED.load(std::sync::atomic::Ordering::SeqCst));

        let started = svc.install_update("v99.0.0".into()).await.unwrap();
        assert_eq!(started.self_update.state, SelfUpdateState::Downloading);
        assert_eq!(started.self_update.version.as_deref(), Some("99.0.0"));
        let done = wait_until_done(&svc).await;
        assert_eq!(done.state, SelfUpdateState::Restarting, "{done:?}");
        let backup = done.backup.expect("a backup is made first");
        assert!(dir.join("data/backups").join(&backup).is_file());

        assert_eq!(
            std::fs::read_to_string(dir.join(BINARY)).unwrap(),
            "binary 99.0.0"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("web/index.html")).unwrap(),
            "99.0.0"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("docs/guide.md")).unwrap(),
            "docs"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("{BINARY}.old"))).unwrap(),
            "binary old"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(INSTALL_MARKER)).unwrap(),
            r#"{"kind": "portable"}"#
        );
        assert!(
            dir.join("data/reelix.db").is_file(),
            "the data folder is untouched"
        );
        assert!(!self_update::staging_dir(&dir, "99.0.0").exists());
        assert!(crate::lifecycle::RESTART_REQUESTED.load(std::sync::atomic::Ordering::SeqCst));
        assert!(crate::lifecycle::shutdown_token().is_cancelled());
        assert_eq!(crate::lifecycle::take_restart(), Some(dir.join(BINARY)));

        let err = svc.install_update("99.0.0".into()).await.unwrap_err();
        assert_eq!(
            err.status(),
            axum::http::StatusCode::CONFLICT,
            "already installing"
        );
    }
}
