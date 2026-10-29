//! M7 integrations: import list exclusions + sync, media server availability.
//!
//! The work is done by a server-side [`IntegrationsBackend`] registered as the [`Integrations`]
//! extension (the API crate cannot depend on the server's wiring).

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use futures::future::BoxFuture;
use reelix_api_types::integrations::{
    BulkIdsRequest, CreateExclusionRequest, ImportListExclusionDto, ImportListStatusDto,
    ImportListSyncRequest, MediaServerAvailabilityDto, MediaServerSyncDto,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob};
use reelix_core::Permissions;
use reelix_db::NewJob;
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;

/// Job kind of the import list sync (payload [`ImportListSyncRequest`]).
pub const IMPORT_LIST_SYNC: &str = "import_list_sync";

/// Server-side implementation of the integration endpoints.
pub trait IntegrationsBackend: Send + Sync {
    fn exclusions(&self) -> BoxFuture<'_, ApiResult<Vec<ImportListExclusionDto>>>;
    /// `Conflict` when the media is already excluded.
    fn add_exclusion(&self, req: CreateExclusionRequest) -> BoxFuture<'_, ApiResult<ImportListExclusionDto>>;
    /// Deletes the given exclusions (empty = all); returns how many were removed.
    fn delete_exclusions(&self, ids: Vec<i64>) -> BoxFuture<'_, ApiResult<u64>>;
    fn list_status(&self) -> BoxFuture<'_, ApiResult<Vec<ImportListStatusDto>>>;
    fn availability(&self) -> BoxFuture<'_, ApiResult<MediaServerAvailabilityDto>>;
}

/// Extension wrapper for the registered [`IntegrationsBackend`].
pub struct Integrations(pub Arc<dyn IntegrationsBackend>);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/importlists/exclusions",
            get(list_exclusions)
                .post(create_exclusion)
                .delete(delete_exclusions),
        )
        .route("/importlists/exclusions/{id}", delete(delete_exclusion))
        .route("/importlists/sync", post(sync_lists))
        .route("/importlists/status", get(list_status))
        .route("/mediaservers/availability", get(availability))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_exclusions,
        create_exclusion,
        delete_exclusion,
        delete_exclusions,
        sync_lists,
        list_status,
        availability
    ),
    components(schemas(
        ImportListExclusionDto,
        CreateExclusionRequest,
        ImportListSyncRequest,
        ImportListStatusDto,
        MediaServerAvailabilityDto,
        MediaServerSyncDto,
        BulkIdsRequest,
    ))
)]
pub struct Api;

fn backend(state: &AppState) -> ApiResult<Arc<dyn IntegrationsBackend>> {
    Ok(state.ext::<Integrations>()?.0.clone())
}

/// Import list exclusions, newest first.
#[utoipa::path(get, path = "/api/v1/importlists/exclusions", tag = "integrations",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Exclusions", body = Vec<ImportListExclusionDto>),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn list_exclusions(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<ImportListExclusionDto>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.exclusions().await?))
}

/// Excludes a movie or series from every import list.
#[utoipa::path(post, path = "/api/v1/importlists/exclusions", tag = "integrations",
    request_body = CreateExclusionRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = ImportListExclusionDto),
        (status = 400, description = "Invalid input", body = ApiErrorBody),
        (status = 409, description = "Already excluded", body = ApiErrorBody),
    ))]
pub async fn create_exclusion(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<CreateExclusionRequest>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    if req.tmdb_id == 0 {
        return Err(ApiError::validation("tmdb_id is required"));
    }
    if req.title.trim().is_empty() {
        return Err(ApiError::validation("title is required"));
    }
    let dto = backend(&state)?.add_exclusion(req).await?;
    state.events().publish(reelix_core::DomainEvent::EntityChanged {
        entity: "import_list_exclusion".into(),
        action: "created".into(),
        id: dto.id,
        user_id: None,
    });
    Ok((StatusCode::CREATED, ApiJson(dto)).into_response())
}

/// Removes one exclusion.
#[utoipa::path(delete, path = "/api/v1/importlists/exclusions/{id}", tag = "integrations",
    params(("id" = i64, Path, description = "Exclusion id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_exclusion(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    if backend(&state)?.delete_exclusions(vec![id]).await? == 0 {
        return Err(ApiError::not_found("exclusion not found"));
    }
    publish_deleted(&state, id);
    Ok(StatusCode::NO_CONTENT)
}

/// Removes several exclusions (`{"ids": [...]}`); an empty list or body removes all.
#[utoipa::path(delete, path = "/api/v1/importlists/exclusions", tag = "integrations",
    request_body(content = Option<BulkIdsRequest>, description = "Ids to delete; empty = all"),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid body", body = ApiErrorBody)))]
pub async fn delete_exclusions(
    State(state): State<AppState>,
    user: CurrentUser,
    body: Bytes,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    let req: BulkIdsRequest = if body.iter().all(u8::is_ascii_whitespace) {
        BulkIdsRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::validation(format!("invalid body: {e}")))?
    };
    backend(&state)?.delete_exclusions(req.ids).await?;
    publish_deleted(&state, 0);
    Ok(StatusCode::NO_CONTENT)
}

fn publish_deleted(state: &AppState, id: i64) {
    state.events().publish(reelix_core::DomainEvent::EntityChanged {
        entity: "import_list_exclusion".into(),
        action: "deleted".into(),
        id,
        user_id: None,
    });
}

/// Syncs import lists now (all enabled lists, or the given ones), ignoring their intervals.
#[utoipa::path(post, path = "/api/v1/importlists/sync", tag = "integrations",
    request_body(content = Option<ImportListSyncRequest>, description = "Lists to sync; empty = all"),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Sync queued", body = EnqueuedJob),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn sync_lists(
    State(state): State<AppState>,
    user: CurrentUser,
    body: Bytes,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let req: ImportListSyncRequest = if body.iter().all(u8::is_ascii_whitespace) {
        ImportListSyncRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::validation(format!("invalid body: {e}")))?
    };
    let mut payload = serde_json::to_value(&req).map_err(ApiError::internal)?;
    payload["force"] = serde_json::Value::Bool(true);
    let job_id = state
        .jobs()
        .enqueue(NewJob::new(IMPORT_LIST_SYNC, payload).priority(5))
        .await?;
    Ok((StatusCode::ACCEPTED, ApiJson(EnqueuedJob { job_id })).into_response())
}

/// Last sync outcome of every import list that has synced at least once.
#[utoipa::path(get, path = "/api/v1/importlists/status", tag = "integrations",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Per-list status", body = Vec<ImportListStatusDto>),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn list_status(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<ImportListStatusDto>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.list_status().await?))
}

/// Which library movies/series exist on the configured media servers (from the last sync).
#[utoipa::path(get, path = "/api/v1/mediaservers/availability", tag = "integrations",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Availability summary", body = MediaServerAvailabilityDto),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn availability(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<MediaServerAvailabilityDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(backend(&state)?.availability().await?))
}
