//! Pipeline activity side (M4): queue, history and manual import. Everything needs
//! `MANAGE_LIBRARY`; reading the queue also accepts `VIEW_QUEUE`. Deleting history and the
//! history settings need `ADMIN`. DTOs: `reelix_api_types::pipeline`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use reelix_api_types::pipeline::{
    HistoryDeleteQuery, HistoryDeleteResult, HistoryDto, HistoryQuery, HistorySettings, ManualImportItemDto,
    ManualImportQuery, ManualImportRequest, PageQuery, Paged, QueueBulkFailure, QueueBulkRemove,
    QueueBulkRemoveResult, QueueItemDto, QueueRemoveQuery,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob, HealthCheck, HealthLevel};
use reelix_core::Permissions;
use reelix_pipeline::{ActivityError, ActivityService};
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::routes::system::HealthSource;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/queue", get(queue))
        .route("/queue/bulk", delete(remove_queue_items))
        .route("/queue/{id}", delete(remove_queue_item))
        .route("/queue/{id}/import", post(retry_import))
        .route("/queue/{id}/ignore", post(ignore_queue_item))
        .route("/history", get(history).delete(delete_history))
        .route("/history/{id}/failed", post(mark_failed))
        .route("/manualimport", get(manual_import_preview).post(manual_import))
        .route(
            "/settings/history",
            get(get_history_settings).put(put_history_settings),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        queue,
        remove_queue_item,
        remove_queue_items,
        retry_import,
        ignore_queue_item,
        history,
        delete_history,
        get_history_settings,
        put_history_settings,
        mark_failed,
        manual_import_preview,
        manual_import,
    ),
    components(schemas(
        QueueItemDto,
        QueueBulkRemove,
        QueueBulkRemoveResult,
        QueueBulkFailure,
        HistoryDto,
        HistoryDeleteResult,
        HistorySettings,
        ManualImportItemDto,
        ManualImportRequest,
        reelix_api_types::pipeline::ManualImportFile,
        reelix_api_types::pipeline::FormatRefDto,
        reelix_api_types::pipeline::MediaRefDto,
        reelix_api_types::pipeline::EpisodeRefDto,
    ))
)]
pub struct Api;

impl From<ActivityError> for ApiError {
    fn from(e: ActivityError) -> Self {
        match e {
            ActivityError::NotFound(m) => ApiError::not_found(m),
            ActivityError::Validation(m) => ApiError::validation(m),
            ActivityError::Conflict(m) => ApiError::Conflict(m),
            ActivityError::Library(l) => l.into(),
            ActivityError::Other(m) => ApiError::Validation {
                status: StatusCode::BAD_GATEWAY,
                message: m,
            },
            other => ApiError::internal(other),
        }
    }
}

/// Health findings of the download monitor (register in `HealthSources`).
pub struct ActivityHealth(pub Arc<ActivityService>);

impl HealthSource for ActivityHealth {
    fn checks(&self) -> Vec<HealthCheck> {
        self.0
            .health_messages()
            .into_iter()
            .map(|message| HealthCheck {
                source: "downloads".into(),
                level: HealthLevel::Warning,
                message,
            })
            .collect()
    }
}

fn svc(state: &AppState) -> ApiResult<Arc<ActivityService>> {
    state.ext::<ActivityService>()
}

fn require(user: &CurrentUser) -> ApiResult<()> {
    user.require(Permissions::MANAGE_LIBRARY)
}

fn accepted(job_id: Option<i64>) -> Response {
    (StatusCode::ACCEPTED, ApiJson(EnqueuedJob { job_id })).into_response()
}

/// Downloads in progress or waiting for import, with live client progress.
#[utoipa::path(get, path = "/api/v1/queue", tag = "pipeline", params(PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Queue (page_size omitted or 0 = all)", body = Paged<QueueItemDto>),
        (status = 403, description = "Requires MANAGE_LIBRARY or VIEW_QUEUE", body = ApiErrorBody),
    ))]
pub async fn queue(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<Paged<QueueItemDto>>> {
    if !user
        .user
        .permissions
        .has_any(Permissions::MANAGE_LIBRARY | Permissions::VIEW_QUEUE)
    {
        return Err(ApiError::Forbidden);
    }
    Ok(ApiJson(svc(&state)?.queue(q.page, q.page_size).await?))
}

/// Removes a queue item; optionally from the client too, and/or blocklists the release
/// (searching for a replacement unless `skip_redownload`).
#[utoipa::path(delete, path = "/api/v1/queue/{id}", tag = "pipeline",
    params(("id" = i64, Path, description = "Queue item id"), QueueRemoveQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Removed"),
        (status = 400, description = "Download client unavailable", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
        (status = 502, description = "The client refused", body = ApiErrorBody),
    ))]
pub async fn remove_queue_item(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<QueueRemoveQuery>,
) -> ApiResult<StatusCode> {
    require(&user)?;
    svc(&state)?.remove(id, &q).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes several queue items with the same options. Each item is handled like
/// `DELETE /queue/{id}`; failures are reported per item and don't stop the rest.
#[utoipa::path(delete, path = "/api/v1/queue/bulk", tag = "pipeline",
    request_body = QueueBulkRemove,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Removed; see `failed` for items that were not", body = QueueBulkRemoveResult),
        (status = 400, description = "Too many ids", body = ApiErrorBody),
    ))]
pub async fn remove_queue_items(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<QueueBulkRemove>,
) -> ApiResult<ApiJson<QueueBulkRemoveResult>> {
    require(&user)?;
    Ok(ApiJson(svc(&state)?.remove_many(&req).await?))
}

/// Retries the automatic import of a completed download (job `process_download`).
#[utoipa::path(post, path = "/api/v1/queue/{id}/import", tag = "pipeline",
    params(("id" = i64, Path, description = "Queue item id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued", body = EnqueuedJob),
        (status = 404, description = "Not found", body = ApiErrorBody),
        (status = 409, description = "Already importing", body = ApiErrorBody),
    ))]
pub async fn retry_import(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require(&user)?;
    Ok(accepted(svc(&state)?.retry_import(id).await?))
}

/// Stops tracking a download without importing it (history `ignored`).
#[utoipa::path(post, path = "/api/v1/queue/{id}/ignore", tag = "pipeline",
    params(("id" = i64, Path, description = "Queue item id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Ignored"),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn ignore_queue_item(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require(&user)?;
    svc(&state)?.ignore(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Grab / import / failure history, newest first (page size default 50, max 500).
#[utoipa::path(get, path = "/api/v1/history", tag = "pipeline", params(HistoryQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "History", body = Paged<HistoryDto>),
        (status = 400, description = "Unknown event_type", body = ApiErrorBody),
    ))]
pub async fn history(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<HistoryQuery>,
) -> ApiResult<ApiJson<Paged<HistoryDto>>> {
    require(&user)?;
    Ok(ApiJson(svc(&state)?.history(&q).await?))
}

/// Deletes every history row dated before `before` (admin). Reelix also does this once a
/// day for rows older than the days to keep (`/settings/history`).
#[utoipa::path(delete, path = "/api/v1/history", tag = "pipeline", params(HistoryDeleteQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Deleted", body = HistoryDeleteResult),
        (status = 400, description = "Missing or invalid `before`", body = ApiErrorBody),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn delete_history(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<HistoryDeleteQuery>,
) -> ApiResult<ApiJson<HistoryDeleteResult>> {
    user.require(Permissions::ADMIN)?;
    let removed = svc(&state)?.delete_history_before(q.before).await?;
    Ok(ApiJson(HistoryDeleteResult { removed }))
}

/// How many days of history to keep (admin). Default 90; 0 keeps everything.
#[utoipa::path(get, path = "/api/v1/settings/history", tag = "pipeline",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "History settings", body = HistorySettings),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn get_history_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<HistorySettings>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.history_settings().await?))
}

/// Sets how many days of history to keep (admin; 0 to 36500, 0 keeps everything). The daily
/// task "Clean up history" removes older rows.
#[utoipa::path(put, path = "/api/v1/settings/history", tag = "pipeline",
    request_body = HistorySettings,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = HistorySettings),
        (status = 400, description = "keep_days out of range", body = ApiErrorBody),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn put_history_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(settings): ApiJson<HistorySettings>,
) -> ApiResult<ApiJson<HistorySettings>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_history_settings(settings).await?))
}

/// Marks a grabbed release as failed: history `download_failed`, blocklist, re-search.
#[utoipa::path(post, path = "/api/v1/history/{id}/failed", tag = "pipeline",
    params(("id" = i64, Path, description = "History id (a `grabbed` row)")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Marked as failed"),
        (status = 400, description = "Not a grabbed row", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn mark_failed(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require(&user)?;
    svc(&state)?.mark_failed(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Videos in a folder (or a queue item's output) with their proposed mapping and rejections.
#[utoipa::path(get, path = "/api/v1/manualimport", tag = "pipeline", params(ManualImportQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Files", body = Vec<ManualImportItemDto>),
        (status = 400, description = "Folder not allowed / missing", body = ApiErrorBody),
        (status = 404, description = "Unknown download", body = ApiErrorBody),
    ))]
pub async fn manual_import_preview(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<ManualImportQuery>,
) -> ApiResult<ApiJson<Vec<ManualImportItemDto>>> {
    require(&user)?;
    Ok(ApiJson(svc(&state)?.manual_import_preview(&q).await?))
}

/// Imports the given files (job `manual_import`).
#[utoipa::path(post, path = "/api/v1/manualimport", tag = "pipeline", request_body = ManualImportRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued", body = EnqueuedJob),
        (status = 400, description = "Invalid request", body = ApiErrorBody),
    ))]
pub async fn manual_import(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<ManualImportRequest>,
) -> ApiResult<Response> {
    require(&user)?;
    Ok(accepted(svc(&state)?.queue_manual_import(&req).await?))
}
