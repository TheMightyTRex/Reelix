//! `/system/migrate`: import settings and libraries from Radarr, Sonarr, Prowlarr, Readarr,
//! Lidarr and Jellyseerr (admin only).

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use reelix_api_types::ApiErrorBody;
use reelix_api_types::migrate::{
    MigrateConnection, MigratePlan, MigrateRunDto, MigrateRunRequest, PendingMigrationSource, SourceKind,
};
use reelix_core::Permissions;
use reelix_migrate::{MigrateError, MigrateService, pending};
use serde::Serialize;
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/system/migrate", get(status))
        .route("/system/migrate/test", post(test))
        .route("/system/migrate/preview", post(preview))
        .route("/system/migrate/run", post(run))
        .route("/system/migrate/pending", get(list_pending).delete(clear_pending))
        .route("/system/migrate/pending/{source}", delete(remove_pending))
}

#[derive(OpenApi)]
#[openapi(
    paths(status, test, preview, run, list_pending, clear_pending, remove_pending),
    components(schemas(
        PendingMigrationSource,
        MigrateConnection,
        MigratePlan,
        MigrateRunDto,
        MigrateRunRequest,
        MigrateTestResult,
        reelix_api_types::migrate::SourceKind,
        reelix_api_types::migrate::MigrateCategory,
        reelix_api_types::migrate::MigrateAction,
        reelix_api_types::migrate::MigrateItem,
        reelix_api_types::migrate::MigrateState,
        reelix_api_types::migrate::PathRewrite,
    ))
)]
pub struct Api;

/// Result of a connection test.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MigrateTestResult {
    pub version: String,
}

fn svc(state: &AppState) -> ApiResult<Arc<MigrateService>> {
    state.ext::<MigrateService>()
}

impl From<MigrateError> for ApiError {
    fn from(e: MigrateError) -> Self {
        match e {
            MigrateError::Input(m) => ApiError::validation(m),
            MigrateError::Source(m) => ApiError::Validation {
                status: StatusCode::BAD_GATEWAY,
                message: m,
            },
            MigrateError::Busy => ApiError::Conflict(e.to_string()),
            MigrateError::Internal(m) => ApiError::Internal(m),
        }
    }
}

/// The current or last migration run (`null` when none has run since the server started).
#[utoipa::path(get, path = "/api/v1/system/migrate", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Current or last run", body = Option<MigrateRunDto>),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn status(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Option<MigrateRunDto>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.status()))
}

/// Checks that Reelix can reach the app with this URL and API key.
#[utoipa::path(post, path = "/api/v1/system/migrate/test", tag = "system",
    request_body = MigrateConnection,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Connected", body = MigrateTestResult),
        (status = 400, description = "Bad URL, key or app", body = ApiErrorBody),
        (status = 502, description = "The app couldn't be reached", body = ApiErrorBody),
    ))]
pub async fn test(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(mut c): ApiJson<MigrateConnection>,
) -> ApiResult<ApiJson<MigrateTestResult>> {
    user.require(Permissions::ADMIN)?;
    pending::resolve_key(state.db(), state.secrets(), &mut c).await?;
    let version = svc(&state)?.test(&c).await?;
    Ok(ApiJson(MigrateTestResult { version }))
}

/// What a migration would create, reuse and skip. Changes nothing.
#[utoipa::path(post, path = "/api/v1/system/migrate/preview", tag = "system",
    request_body = MigrateConnection,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Plan", body = MigratePlan),
        (status = 502, description = "The app couldn't be read", body = ApiErrorBody),
    ))]
pub async fn preview(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(mut c): ApiJson<MigrateConnection>,
) -> ApiResult<ApiJson<MigratePlan>> {
    user.require(Permissions::ADMIN)?;
    pending::resolve_key(state.db(), state.secrets(), &mut c).await?;
    Ok(ApiJson(svc(&state)?.preview(&c).await?))
}

/// Starts a migration in the background; poll `GET /system/migrate` for progress.
#[utoipa::path(post, path = "/api/v1/system/migrate/run", tag = "system",
    request_body = MigrateRunRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Started", body = MigrateRunDto),
        (status = 409, description = "A migration is already running", body = ApiErrorBody),
    ))]
pub async fn run(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(mut req): ApiJson<MigrateRunRequest>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let saved = pending::resolve_key(state.db(), state.secrets(), &mut req.connection).await?;
    let source = req.connection.source;
    let started = svc(&state)?.start(req)?;
    // The run has the key now; the saved source has done its job.
    if saved {
        pending::remove(state.db(), Some(source)).await?;
    }
    tracing::info!(source = started.source.as_str(), by = %user.user.username, "migration started");
    Ok((StatusCode::ACCEPTED, ApiJson(started)).into_response())
}

/// Apps Setup saved to migrate from (API keys masked). Send `api_key: "********"` with the same
/// source and URL to `test`, `preview` or `run` to use the saved key.
#[utoipa::path(get, path = "/api/v1/system/migrate/pending", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved sources", body = Vec<PendingMigrationSource>),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn list_pending(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<PendingMigrationSource>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(pending::list(state.db()).await?))
}

/// Forgets every saved source.
#[utoipa::path(delete, path = "/api/v1/system/migrate/pending", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Cleared"),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn clear_pending(State(state): State<AppState>, user: CurrentUser) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    pending::remove(state.db(), None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Forgets the saved source(s) of one app.
#[utoipa::path(delete, path = "/api/v1/system/migrate/pending/{source}", tag = "system",
    params(("source" = SourceKind, Path, description = "radarr, sonarr, prowlarr, readarr, lidarr or jellyseerr")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "Admin only", body = ApiErrorBody),
        (status = 404, description = "Nothing saved for this app", body = ApiErrorBody),
    ))]
pub async fn remove_pending(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(source): Path<String>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    let source =
        SourceKind::parse(&source).ok_or_else(|| ApiError::NotFound(format!("unknown app {source:?}")))?;
    if pending::remove(state.db(), Some(source)).await? == 0 {
        return Err(ApiError::NotFound(format!(
            "nothing is saved for {}",
            source.label()
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}
