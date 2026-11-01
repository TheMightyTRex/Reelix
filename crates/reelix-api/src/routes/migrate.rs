//! `/system/migrate`: import settings and libraries from Radarr, Sonarr, Prowlarr, Readarr,
//! Lidarr and Jellyseerr (admin only).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use reelix_api_types::ApiErrorBody;
use reelix_api_types::migrate::{MigrateConnection, MigratePlan, MigrateRunDto, MigrateRunRequest};
use reelix_core::Permissions;
use reelix_migrate::{MigrateError, MigrateService};
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
}

#[derive(OpenApi)]
#[openapi(
    paths(status, test, preview, run),
    components(schemas(
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
    ApiJson(c): ApiJson<MigrateConnection>,
) -> ApiResult<ApiJson<MigrateTestResult>> {
    user.require(Permissions::ADMIN)?;
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
    ApiJson(c): ApiJson<MigrateConnection>,
) -> ApiResult<ApiJson<MigratePlan>> {
    user.require(Permissions::ADMIN)?;
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
    ApiJson(req): ApiJson<MigrateRunRequest>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let started = svc(&state)?.start(req)?;
    tracing::info!(source = started.source.as_str(), by = %user.user.username, "migration started");
    Ok((StatusCode::ACCEPTED, ApiJson(started)).into_response())
}
