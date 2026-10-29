//! Series, seasons, episodes and episode files (`/series`, `/episodes`, `/seasons`, `/episodefiles`).

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post, put};
use reelix_api_types::library::{
    AddSeriesRequest, BulkDeleteRequest, DeleteQuery, EpisodeDto, EpisodeFileDto, EpisodeFileQuery,
    EpisodeListQuery, EpisodeMonitorRequest, LookupQuery, LookupResult, Paged, SeasonMonitorRequest,
    SeasonPassRequest, SeriesBulkEdit, SeriesDto, SeriesEdit, SeriesListQuery,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob};
use utoipa::OpenApi;

use super::{accepted, created, require_lib, svc};
use crate::auth::CurrentUser;
use crate::error::{ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/series", get(list_series).post(add_series))
        .route("/series/bulk", put(bulk_edit_series).delete(bulk_delete_series))
        .route("/series/lookup", get(lookup_series))
        .route(
            "/series/{id}",
            get(get_series).put(edit_series).delete(delete_series),
        )
        .route("/series/{id}/episodes", get(list_episodes))
        .route("/series/{id}/refresh", post(refresh_series))
        .route("/series/{id}/rescan", post(rescan_series))
        .route("/episodes/monitor", put(monitor_episodes))
        .route("/seasons/monitor", put(monitor_season))
        .route("/seasonpass", post(season_pass))
        .route("/episodefiles", get(list_episode_files))
        .route("/episodefiles/{id}", axum::routing::delete(delete_episode_file))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_series,
        add_series,
        get_series,
        edit_series,
        delete_series,
        bulk_edit_series,
        bulk_delete_series,
        lookup_series,
        list_episodes,
        refresh_series,
        rescan_series,
        monitor_episodes,
        monitor_season,
        season_pass,
        list_episode_files,
        delete_episode_file,
    ),
    components(schemas(
        reelix_api_types::library::SeriesDto,
        reelix_api_types::library::SeasonDto,
        reelix_api_types::library::SeasonStatistics,
        reelix_api_types::library::SeriesStatistics,
        reelix_api_types::library::SeriesStatus,
        reelix_api_types::library::SeriesType,
        reelix_api_types::library::SeriesMonitor,
        reelix_api_types::library::NewItemMonitor,
        reelix_api_types::library::AddSeriesRequest,
        reelix_api_types::library::SeriesEdit,
        reelix_api_types::library::SeriesBulkEdit,
        reelix_api_types::library::EpisodeDto,
        reelix_api_types::library::EpisodeFileDto,
        reelix_api_types::library::EpisodeMonitorRequest,
        reelix_api_types::library::SeasonMonitorRequest,
        reelix_api_types::library::SeasonPassRequest,
    ))
)]
pub struct Api;

/// Series (filterable, sortable, optionally paged) with season statistics.
#[utoipa::path(get, path = "/api/v1/series", tag = "library", params(SeriesListQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Series", body = Paged<SeriesDto>)))]
pub async fn list_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<SeriesListQuery>,
) -> ApiResult<ApiJson<Paged<SeriesDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.list_series(&q).await?))
}

/// Adds a series from TMDB (seasons + episodes created, monitoring applied).
#[utoipa::path(post, path = "/api/v1/series", tag = "library", request_body = AddSeriesRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Added", body = SeriesDto),
        (status = 409, description = "Already in the library", body = ApiErrorBody),
        (status = 503, description = "TMDB not configured", body = ApiErrorBody),
    ))]
pub async fn add_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AddSeriesRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(created(svc(&state)?.add_series(req).await?))
}

/// One series (with cast).
#[utoipa::path(get, path = "/api/v1/series/{id}", tag = "library",
    params(("id" = i64, Path, description = "Series id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Series", body = SeriesDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<SeriesDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_series(id).await?))
}

/// Edits a series (partial).
#[utoipa::path(put, path = "/api/v1/series/{id}", tag = "library", request_body = SeriesEdit,
    params(("id" = i64, Path, description = "Series id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = SeriesDto)))]
pub async fn edit_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(edit): ApiJson<SeriesEdit>,
) -> ApiResult<ApiJson<SeriesDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.update_series(id, edit).await?))
}

/// Deletes a series (`?delete_files=true` also removes its folder).
#[utoipa::path(delete, path = "/api/v1/series/{id}", tag = "library",
    params(("id" = i64, Path, description = "Series id"), DeleteQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted")))]
pub async fn delete_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<DeleteQuery>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_series(id, q.delete_files).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Bulk edit series.
#[utoipa::path(put, path = "/api/v1/series/bulk", tag = "library", request_body = SeriesBulkEdit,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = Vec<SeriesDto>)))]
pub async fn bulk_edit_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<SeriesBulkEdit>,
) -> ApiResult<ApiJson<Vec<SeriesDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.bulk_update_series(req).await?))
}

/// Bulk delete series.
#[utoipa::path(delete, path = "/api/v1/series/bulk", tag = "library", request_body = BulkDeleteRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted")))]
pub async fn bulk_delete_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<BulkDeleteRequest>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.bulk_delete_series(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// TMDB series search with in-library flags (`tmdb:1399`, `tvdb:121361`, `imdb:tt…`).
#[utoipa::path(get, path = "/api/v1/series/lookup", tag = "library", params(LookupQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Results", body = Vec<LookupResult>)))]
pub async fn lookup_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<LookupQuery>,
) -> ApiResult<ApiJson<Vec<LookupResult>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.lookup_series(&q.term).await?))
}

/// Episodes of a series (optionally one season), each with its file.
#[utoipa::path(get, path = "/api/v1/series/{id}/episodes", tag = "library",
    params(("id" = i64, Path, description = "Series id"), EpisodeListQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Episodes", body = Vec<EpisodeDto>)))]
pub async fn list_episodes(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<EpisodeListQuery>,
) -> ApiResult<ApiJson<Vec<EpisodeDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.episodes(id, q.season).await?))
}

/// Queues a metadata refresh of one series.
#[utoipa::path(post, path = "/api/v1/series/{id}/refresh", tag = "library",
    params(("id" = i64, Path, description = "Series id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob)))]
pub async fn refresh_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_refresh_series(id).await?))
}

/// Queues a disk scan of one series.
#[utoipa::path(post, path = "/api/v1/series/{id}/rescan", tag = "library",
    params(("id" = i64, Path, description = "Series id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob)))]
pub async fn rescan_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_rescan_series(id).await?))
}

/// (Un)monitors episodes; returns the changed episodes.
#[utoipa::path(put, path = "/api/v1/episodes/monitor", tag = "library", request_body = EpisodeMonitorRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated episodes", body = Vec<EpisodeDto>)))]
pub async fn monitor_episodes(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<EpisodeMonitorRequest>,
) -> ApiResult<ApiJson<Vec<EpisodeDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.monitor_episodes(req).await?))
}

/// (Un)monitors a season (and by default its episodes); returns the series.
#[utoipa::path(put, path = "/api/v1/seasons/monitor", tag = "library", request_body = SeasonMonitorRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Series", body = SeriesDto)))]
pub async fn monitor_season(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<SeasonMonitorRequest>,
) -> ApiResult<ApiJson<SeriesDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.monitor_season(req).await?))
}

/// Season pass: applies a monitor option to many series.
#[utoipa::path(post, path = "/api/v1/seasonpass", tag = "library", request_body = SeasonPassRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated series", body = Vec<SeriesDto>)))]
pub async fn season_pass(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<SeasonPassRequest>,
) -> ApiResult<ApiJson<Vec<SeriesDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.season_pass(req).await?))
}

/// Episode files of a series.
#[utoipa::path(get, path = "/api/v1/episodefiles", tag = "library", params(EpisodeFileQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Files", body = Vec<EpisodeFileDto>)))]
pub async fn list_episode_files(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<EpisodeFileQuery>,
) -> ApiResult<ApiJson<Vec<EpisodeFileDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.episode_files(q.series_id).await?))
}

/// Deletes an episode file (recycle bin when configured).
#[utoipa::path(delete, path = "/api/v1/episodefiles/{id}", tag = "library",
    params(("id" = i64, Path, description = "Episode file id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_episode_file(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_episode_file(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
