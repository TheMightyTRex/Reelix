//! Pipeline search side (M4): interactive search + grab, blocklist and pending (delayed)
//! releases. Everything needs `MANAGE_LIBRARY`. DTOs: `reelix_api_types::pipeline`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use reelix_api_types::ApiErrorBody;
use reelix_api_types::pipeline::{
    BlocklistDto, BulkIdsRequest, GrabRequest, GrabResponse, PageQuery, Paged, PendingReleaseDto, ReleaseDto,
    ReleaseQuery,
};
use reelix_core::Permissions;
use reelix_pipeline::search::{SearchError, SearchService};
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/releases", get(releases))
        .route("/releases/grab", post(grab))
        .route("/blocklist", get(list_blocklist).delete(clear_blocklist))
        .route("/blocklist/{id}", delete(delete_blocklist))
        .route("/pending", get(list_pending))
        .route("/pending/{id}", delete(delete_pending))
        .route("/pending/{id}/grab", post(grab_pending))
}

#[derive(OpenApi)]
#[openapi(
    paths(releases, grab, list_blocklist, delete_blocklist, clear_blocklist, list_pending, delete_pending, grab_pending),
    components(schemas(
        ReleaseDto, GrabRequest, GrabResponse, BlocklistDto, BulkIdsRequest, PendingReleaseDto,
        reelix_api_types::pipeline::FormatRefDto,
        reelix_api_types::pipeline::MediaRefDto,
        reelix_api_types::pipeline::EpisodeRefDto,
        reelix_api_types::pipeline::RejectionDto,
    )),
    tags((name = "pipeline", description = "Interactive search, grabbing, queue, history, blocklist, pending releases and manual import")),
)]
pub struct Api;

impl From<SearchError> for ApiError {
    fn from(e: SearchError) -> Self {
        match e {
            SearchError::NotFound(m) => ApiError::not_found(m),
            SearchError::Validation(m) => ApiError::validation(m),
            SearchError::Unavailable(m) => ApiError::Validation {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: m,
            },
            SearchError::Library(l) => l.into(),
            SearchError::Db(d) => ApiError::internal(d),
            SearchError::Internal(m) => ApiError::internal(m),
        }
    }
}

fn svc(state: &AppState) -> ApiResult<Arc<SearchService>> {
    state.ext::<SearchService>()
}

fn require(user: &CurrentUser) -> ApiResult<()> {
    user.require(Permissions::MANAGE_LIBRARY)
}

/// Interactive search for a movie, season, episode, book format (`book_id` + optional
/// `book_format`) or album: every release from interactive-enabled indexers, with its decision.
#[utoipa::path(get, path = "/api/v1/releases", tag = "pipeline", params(ReleaseQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Releases, best first", body = Vec<ReleaseDto>),
        (status = 400, description = "Missing target", body = ApiErrorBody),
        (status = 404, description = "Unknown media", body = ApiErrorBody),
    ))]
pub async fn releases(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<ReleaseQuery>,
) -> ApiResult<ApiJson<Vec<ReleaseDto>>> {
    require(&user)?;
    Ok(ApiJson(svc(&state)?.interactive(&q).await?))
}

/// Grabs an interactive-search result (or a raw `download_url` from the indexer search page).
#[utoipa::path(post, path = "/api/v1/releases/grab", tag = "pipeline", request_body = GrabRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Sent to the download client", body = GrabResponse),
        (status = 400, description = "Rejected release (use force)", body = ApiErrorBody),
        (status = 404, description = "Release no longer cached", body = ApiErrorBody),
        (status = 503, description = "No usable download client / client refused", body = ApiErrorBody),
    ))]
pub async fn grab(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<GrabRequest>,
) -> ApiResult<ApiJson<GrabResponse>> {
    require(&user)?;
    Ok(ApiJson(svc(&state)?.grab_request(&req).await?))
}

/// Blocklisted releases, newest first.
#[utoipa::path(get, path = "/api/v1/blocklist", tag = "pipeline", params(PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Blocklist", body = Paged<BlocklistDto>)))]
pub async fn list_blocklist(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<Paged<BlocklistDto>>> {
    require(&user)?;
    Ok(ApiJson(svc(&state)?.list_blocklist(q.page, q.page_size).await?))
}

/// Removes one blocklist entry.
#[utoipa::path(delete, path = "/api/v1/blocklist/{id}", tag = "pipeline",
    params(("id" = i64, Path, description = "Blocklist id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_blocklist(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require(&user)?;
    svc(&state)?.delete_blocklist(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes the given entries (`{ids}`), or the whole blocklist when `ids` is empty / no body.
#[utoipa::path(delete, path = "/api/v1/blocklist", tag = "pipeline", request_body = Option<BulkIdsRequest>,
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Removed")))]
pub async fn clear_blocklist(
    State(state): State<AppState>,
    user: CurrentUser,
    body: axum::body::Bytes,
) -> ApiResult<StatusCode> {
    require(&user)?;
    let ids = if body.iter().all(u8::is_ascii_whitespace) {
        Vec::new()
    } else {
        serde_json::from_slice::<BulkIdsRequest>(&body)
            .map_err(|e| ApiError::validation(format!("invalid body: {e}")))?
            .ids
    };
    svc(&state)?.clear_blocklist(&ids).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Releases held back by a delay profile.
#[utoipa::path(get, path = "/api/v1/pending", tag = "pipeline",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Pending releases", body = Vec<PendingReleaseDto>)))]
pub async fn list_pending(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<PendingReleaseDto>>> {
    require(&user)?;
    Ok(ApiJson(svc(&state)?.list_pending().await?))
}

/// Discards a pending release.
#[utoipa::path(delete, path = "/api/v1/pending/{id}", tag = "pipeline",
    params(("id" = i64, Path, description = "Pending release id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_pending(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require(&user)?;
    svc(&state)?.delete_pending(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Grabs a pending release now.
#[utoipa::path(post, path = "/api/v1/pending/{id}/grab", tag = "pipeline",
    params(("id" = i64, Path, description = "Pending release id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Grabbed", body = GrabResponse),
        (status = 404, description = "Not found", body = ApiErrorBody),
        (status = 503, description = "No usable download client", body = ApiErrorBody),
    ))]
pub async fn grab_pending(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<GrabResponse>> {
    require(&user)?;
    Ok(ApiJson(svc(&state)?.grab_pending(id).await?))
}
