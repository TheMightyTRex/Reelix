//! System maintenance: backups and restore, log viewer, update checks and server settings, plus
//! the signed-in user's UI settings.
//!
//! The file and network work is done by a server-side [`MaintenanceBackend`] registered as the
//! [`Maintenance`] extension (it needs the server's resolved paths and configuration).

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use futures::future::BoxFuture;
use reelix_api_types::ApiErrorBody;
use reelix_api_types::system::{
    BackupDto, BackupKind, BackupSettings, BackupsOverview, CalendarView, DateFormat, FirstDayOfWeek,
    InstallKind, LibraryView, LogEntryDto, LogFileDto, LogLevel, LogsDto, PendingRestoreDto, ReleaseAssetDto,
    ReleaseDto, ServerSettingsDto, ServerSettingsValues, TimeFormat, UiSettings, UpdateSettings,
    UpdateStatusDto,
};
use reelix_core::Permissions;
use serde::Deserialize;
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Largest backup archive accepted by the restore upload.
pub const MAX_BACKUP_UPLOAD: usize = 2 * 1024 * 1024 * 1024;

/// What the log viewer asks for.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LogsQuery {
    /// Log file name (default: the newest file).
    pub file: Option<String>,
    /// Lowest level to include (default: `info`).
    pub level: Option<String>,
    /// Case-insensitive text the entry must contain.
    pub search: Option<String>,
    /// Maximum number of entries (1-5000, default 500).
    pub limit: Option<usize>,
}

/// Server-side implementation of the maintenance endpoints.
pub trait MaintenanceBackend: Send + Sync {
    fn backups(&self) -> BoxFuture<'_, ApiResult<BackupsOverview>>;
    fn create_backup(&self, kind: BackupKind) -> BoxFuture<'_, ApiResult<BackupDto>>;
    /// Path of an existing backup; `NotFound` for unknown or unsafe names.
    fn backup_path(&self, name: &str) -> ApiResult<PathBuf>;
    fn delete_backup(&self, name: String) -> BoxFuture<'_, ApiResult<()>>;
    fn set_backup_settings(&self, settings: BackupSettings) -> BoxFuture<'_, ApiResult<BackupSettings>>;
    /// Validates the backup and stages it to replace the database on the next start.
    fn stage_restore(&self, name: String) -> BoxFuture<'_, ApiResult<PendingRestoreDto>>;
    fn stage_restore_upload(&self, archive: Bytes) -> BoxFuture<'_, ApiResult<PendingRestoreDto>>;
    fn cancel_restore(&self) -> BoxFuture<'_, ApiResult<()>>;

    fn log_files(&self) -> BoxFuture<'_, ApiResult<Vec<LogFileDto>>>;
    fn log_path(&self, name: &str) -> ApiResult<PathBuf>;
    fn logs(&self, query: LogsQuery) -> BoxFuture<'_, ApiResult<LogsDto>>;

    /// Cached status; `force` checks GitHub now.
    fn update_status(&self, force: bool) -> BoxFuture<'_, ApiResult<UpdateStatusDto>>;
    fn set_update_settings(&self, settings: UpdateSettings) -> BoxFuture<'_, ApiResult<UpdateStatusDto>>;

    fn server_settings(&self) -> BoxFuture<'_, ApiResult<ServerSettingsDto>>;
    fn set_server_settings(
        &self,
        values: ServerSettingsValues,
    ) -> BoxFuture<'_, ApiResult<ServerSettingsDto>>;
}

/// Extension wrapper for the registered [`MaintenanceBackend`].
pub struct Maintenance(pub Arc<dyn MaintenanceBackend>);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/system/backups", get(list_backups).post(create_backup))
        .route("/system/backups/settings", put(update_backup_settings))
        .route(
            "/system/backups/restore",
            post(restore_upload)
                .delete(cancel_restore)
                .layer(DefaultBodyLimit::max(MAX_BACKUP_UPLOAD)),
        )
        .route(
            "/system/backups/{name}",
            get(download_backup).delete(delete_backup),
        )
        .route("/system/backups/{name}/restore", post(restore_backup))
        .route("/system/logs", get(logs))
        .route("/system/logs/files", get(log_files))
        .route("/system/logs/files/{name}", get(download_log))
        .route("/system/updates", get(update_status))
        .route("/system/updates/check", post(check_updates))
        .route("/system/updates/settings", put(update_update_settings))
        .route(
            "/system/settings",
            get(server_settings).put(update_server_settings),
        )
        .route("/settings/ui", get(ui_settings).put(update_ui_settings))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_backups,
        create_backup,
        update_backup_settings,
        download_backup,
        delete_backup,
        restore_backup,
        restore_upload,
        cancel_restore,
        logs,
        log_files,
        download_log,
        update_status,
        check_updates,
        update_update_settings,
        server_settings,
        update_server_settings,
        ui_settings,
        update_ui_settings,
    ),
    components(schemas(
        BackupKind,
        BackupDto,
        BackupSettings,
        BackupsOverview,
        PendingRestoreDto,
        LogLevel,
        LogFileDto,
        LogEntryDto,
        LogsDto,
        InstallKind,
        ReleaseAssetDto,
        ReleaseDto,
        UpdateStatusDto,
        UpdateSettings,
        ServerSettingsValues,
        ServerSettingsDto,
        DateFormat,
        TimeFormat,
        FirstDayOfWeek,
        CalendarView,
        LibraryView,
        UiSettings,
    ))
)]
pub struct Api;

fn backend(state: &AppState) -> ApiResult<Arc<dyn MaintenanceBackend>> {
    Ok(state.ext::<Maintenance>()?.0.clone())
}

/// Streams `path` as an attachment called `name`.
async fn file_response(path: PathBuf, name: &str, content_type: &'static str) -> ApiResult<Response> {
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found(format!("'{name}' does not exist")))?;
    let len = file.metadata().await.map(|m| m.len()).ok();
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    let mut res = (StatusCode::OK, body).into_response();
    let headers = res.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Some(len) = len {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    // Names are restricted to [A-Za-z0-9._-] by the backend, so they are safe in the header.
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

// ---------- Backups ----------

/// Backups, schedule and any pending restore (admin).
#[utoipa::path(get, path = "/api/v1/system/backups", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Backups", body = BackupsOverview),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn list_backups(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<BackupsOverview>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.backups().await?))
}

/// Backs up the database, config file and secret key now (admin).
#[utoipa::path(post, path = "/api/v1/system/backups", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created backup", body = BackupDto),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn create_backup(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let backup = backend(&state)?.create_backup(BackupKind::Manual).await?;
    tracing::info!(name = %backup.name, by = %user.user.username, "backup created");
    Ok((StatusCode::CREATED, ApiJson(backup)).into_response())
}

/// Changes the backup schedule and retention (admin).
#[utoipa::path(put, path = "/api/v1/system/backups/settings", tag = "system",
    request_body = BackupSettings,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved settings", body = BackupSettings),
        (status = 400, description = "Invalid settings", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn update_backup_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(body): ApiJson<BackupSettings>,
) -> ApiResult<ApiJson<BackupSettings>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.set_backup_settings(body).await?))
}

/// Downloads a backup archive (admin). It contains the secret key, so store it safely.
#[utoipa::path(get, path = "/api/v1/system/backups/{name}", tag = "system",
    params(("name" = String, Path, description = "Backup file name")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Zip archive", content_type = "application/zip"),
        (status = 403, description = "Admin only", body = ApiErrorBody),
        (status = 404, description = "Unknown backup", body = ApiErrorBody),
    ))]
pub async fn download_backup(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(name): ApiPath<String>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let path = backend(&state)?.backup_path(&name)?;
    file_response(path, &name, "application/zip").await
}

/// Deletes a backup (admin).
#[utoipa::path(delete, path = "/api/v1/system/backups/{name}", tag = "system",
    params(("name" = String, Path, description = "Backup file name")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Admin only", body = ApiErrorBody),
        (status = 404, description = "Unknown backup", body = ApiErrorBody),
    ))]
pub async fn delete_backup(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(name): ApiPath<String>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    backend(&state)?.delete_backup(name.clone()).await?;
    tracing::info!(%name, by = %user.user.username, "backup deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Stages a stored backup to be restored on the next restart (admin).
#[utoipa::path(post, path = "/api/v1/system/backups/{name}/restore", tag = "system",
    params(("name" = String, Path, description = "Backup file name")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Restore staged; restart Reelix to apply it", body = PendingRestoreDto),
        (status = 400, description = "Not a valid Reelix backup", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
        (status = 404, description = "Unknown backup", body = ApiErrorBody),
    ))]
pub async fn restore_backup(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(name): ApiPath<String>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let pending = backend(&state)?.stage_restore(name.clone()).await?;
    tracing::warn!(%name, by = %user.user.username, "restore staged; it is applied on the next restart");
    Ok((StatusCode::ACCEPTED, ApiJson(pending)).into_response())
}

/// Stages an uploaded backup archive (the raw zip as the body) to be restored on the next restart (admin).
#[utoipa::path(post, path = "/api/v1/system/backups/restore", tag = "system",
    request_body(content = Vec<u8>, content_type = "application/zip"),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Restore staged; restart Reelix to apply it", body = PendingRestoreDto),
        (status = 400, description = "Not a valid Reelix backup", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn restore_upload(
    State(state): State<AppState>,
    user: CurrentUser,
    body: Bytes,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    if body.is_empty() {
        return Err(ApiError::validation("upload a backup .zip file"));
    }
    let pending = backend(&state)?.stage_restore_upload(body).await?;
    tracing::warn!(by = %user.user.username, "restore staged from an uploaded backup; it is applied on the next restart");
    Ok((StatusCode::ACCEPTED, ApiJson(pending)).into_response())
}

/// Cancels a staged restore (admin).
#[utoipa::path(delete, path = "/api/v1/system/backups/restore", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Cancelled (or nothing was staged)"),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn cancel_restore(State(state): State<AppState>, user: CurrentUser) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    backend(&state)?.cancel_restore().await?;
    tracing::info!(by = %user.user.username, "staged restore cancelled");
    Ok(StatusCode::NO_CONTENT)
}

// ---------- Logs ----------

/// Recent log entries, newest first (admin).
#[utoipa::path(get, path = "/api/v1/system/logs", tag = "system",
    params(LogsQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Log entries", body = LogsDto),
        (status = 403, description = "Admin only", body = ApiErrorBody),
        (status = 404, description = "Unknown log file", body = ApiErrorBody),
    ))]
pub async fn logs(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<LogsQuery>,
) -> ApiResult<ApiJson<LogsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.logs(q).await?))
}

/// Log files on disk, newest first (admin).
#[utoipa::path(get, path = "/api/v1/system/logs/files", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Log files", body = Vec<LogFileDto>),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn log_files(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<LogFileDto>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.log_files().await?))
}

/// Downloads a log file (admin).
#[utoipa::path(get, path = "/api/v1/system/logs/files/{name}", tag = "system",
    params(("name" = String, Path, description = "Log file name")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Log file", content_type = "text/plain"),
        (status = 403, description = "Admin only", body = ApiErrorBody),
        (status = 404, description = "Unknown log file", body = ApiErrorBody),
    ))]
pub async fn download_log(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(name): ApiPath<String>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let path = backend(&state)?.log_path(&name)?;
    file_response(path, &name, "text/plain; charset=utf-8").await
}

// ---------- Updates ----------

/// Installed version and the latest releases from GitHub (admin; cached).
#[utoipa::path(get, path = "/api/v1/system/updates", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Update status", body = UpdateStatusDto),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn update_status(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<UpdateStatusDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.update_status(false).await?))
}

/// Checks GitHub for new releases now (admin).
#[utoipa::path(post, path = "/api/v1/system/updates/check", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Update status", body = UpdateStatusDto),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn check_updates(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<UpdateStatusDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.update_status(true).await?))
}

/// Turns scheduled update checks on or off (admin).
#[utoipa::path(put, path = "/api/v1/system/updates/settings", tag = "system",
    request_body = UpdateSettings,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Update status", body = UpdateStatusDto),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn update_update_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(body): ApiJson<UpdateSettings>,
) -> ApiResult<ApiJson<UpdateStatusDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.set_update_settings(body).await?))
}

// ---------- Server settings ----------

/// Bind address, port, URL base and logging from config.toml (admin).
#[utoipa::path(get, path = "/api/v1/system/settings", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Server settings", body = ServerSettingsDto),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn server_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<ServerSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.server_settings().await?))
}

/// Saves server settings to config.toml (admin). They take effect after a restart.
#[utoipa::path(put, path = "/api/v1/system/settings", tag = "system",
    request_body = ServerSettingsValues,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved settings", body = ServerSettingsDto),
        (status = 400, description = "Invalid settings", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn update_server_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(body): ApiJson<ServerSettingsValues>,
) -> ApiResult<ApiJson<ServerSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    let saved = backend(&state)?.set_server_settings(body).await?;
    tracing::info!(by = %user.user.username, "server settings saved to config.toml");
    Ok(ApiJson(saved))
}

// ---------- UI settings ----------

fn ui_key(user_id: i64) -> String {
    format!("ui.user.{user_id}")
}

/// The signed-in user's display preferences.
#[utoipa::path(get, path = "/api/v1/settings/ui", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "UI settings", body = UiSettings),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
    ))]
pub async fn ui_settings(State(state): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<UiSettings>> {
    // Unreadable (older) values fall back to the defaults instead of breaking the UI.
    let saved = state
        .db()
        .settings()
        .get::<UiSettings>(&ui_key(user.user.id))
        .await
        .ok()
        .flatten();
    Ok(ApiJson(saved.unwrap_or_default()))
}

/// Saves the signed-in user's display preferences.
#[utoipa::path(put, path = "/api/v1/settings/ui", tag = "system",
    request_body = UiSettings,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved settings", body = UiSettings),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
    ))]
pub async fn update_ui_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(body): ApiJson<UiSettings>,
) -> ApiResult<ApiJson<UiSettings>> {
    state.db().settings().set(&ui_key(user.user.id), &body).await?;
    Ok(ApiJson(body))
}
