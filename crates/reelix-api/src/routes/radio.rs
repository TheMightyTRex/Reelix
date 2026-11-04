//! Radio: shows with their series and episodes, hand-set file matches, provider search and
//! preview, stations, the podcast library review, radio root folders and radio settings.
//!
//! Reads and writes need `MANAGE_LIBRARY`, as for the other media types; radio settings and
//! root-folder changes need `ADMIN`. DTOs: `reelix_api_types::radio`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use reelix_api_types::content::{ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_api_types::library::Paged;
use reelix_api_types::radio::{
    AddRadioShowRequest, RadioClearFileQuery, RadioEpisodeDto, RadioEpisodeFileRequest,
    RadioLibraryReviewDto, RadioPreviewQuery, RadioProviderDto, RadioSearchQuery, RadioSettingsDto,
    RadioShowDto, RadioShowEdit, RadioShowEpisodesDto, RadioShowHitDto, RadioShowListQuery,
    RadioShowPreviewDto, RadioStationDto,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob};
use reelix_core::Permissions;
use reelix_radio::{ProviderError, RadioError, RadioService};
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::routes::library::{accepted, created, require_lib};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/radio/shows", get(list_shows).post(add_show))
        .route(
            "/radio/shows/{id}",
            get(get_show).put(edit_show).delete(delete_show),
        )
        .route("/radio/shows/{id}/episodes", get(show_episodes))
        .route("/radio/shows/{id}/refresh", post(refresh_show))
        .route("/radio/episodes/{id}", get(get_episode))
        .route(
            "/radio/episodes/{id}/file",
            put(set_episode_file).delete(clear_episode_file),
        )
        .route("/radio/search", get(search))
        .route("/radio/preview", get(preview))
        .route("/radio/stations", get(stations))
        .route("/radio/providers", get(providers))
        .route("/radio/library/review", get(library_review))
        .route("/radio/rootfolders", get(list_root_folders).post(add_root_folder))
        .route("/radio/rootfolders/{id}", delete(delete_root_folder))
        .route("/radio/settings", get(get_settings).put(put_settings))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_shows, add_show, get_show, edit_show, delete_show, show_episodes, refresh_show,
        get_episode, set_episode_file, clear_episode_file, search, preview, stations, providers,
        library_review, list_root_folders, add_root_folder, delete_root_folder, get_settings, put_settings,
    ),
    components(schemas(
        reelix_api_types::radio::RadioFollow,
        reelix_api_types::radio::RadioSeriesKind,
        reelix_api_types::radio::RadioNumberSource,
        reelix_api_types::radio::RadioEpisodeStatus,
        reelix_api_types::radio::RadioStationDto,
        reelix_api_types::radio::RadioShowStatisticsDto,
        reelix_api_types::radio::RadioShowDto,
        reelix_api_types::radio::RadioSeriesDto,
        reelix_api_types::radio::RadioEpisodeFileDto,
        reelix_api_types::radio::RadioEpisodeDto,
        reelix_api_types::radio::RadioShowEpisodesDto,
        reelix_api_types::radio::RadioShowHitDto,
        reelix_api_types::radio::RadioShowPreviewDto,
        reelix_api_types::radio::RadioPreviewEpisodeDto,
        reelix_api_types::radio::RadioProviderDto,
        reelix_api_types::radio::AddRadioShowRequest,
        reelix_api_types::radio::RadioShowEdit,
        reelix_api_types::radio::RadioEpisodeFileRequest,
        reelix_api_types::radio::RadioLibraryReviewDto,
        reelix_api_types::radio::RadioLibraryShowDto,
        reelix_api_types::radio::RadioSettingsDto,
        reelix_api_types::content::ContentRootFolderDto,
        reelix_api_types::content::CreateContentRootFolderRequest,
    )),
    tags((name = "radio", description = "Radio shows and podcasts: series, episodes, matches to the Audiobookshelf podcast library, search, preview, stations, root folders and radio settings")),
)]
pub struct Api;

pub(crate) fn svc(state: &AppState) -> ApiResult<Arc<RadioService>> {
    state.ext::<RadioService>()
}

impl From<RadioError> for ApiError {
    fn from(e: RadioError) -> Self {
        match e {
            RadioError::NotFound(m) => ApiError::not_found(m),
            RadioError::Validation(m) => ApiError::validation(m),
            RadioError::Conflict(m) => ApiError::Conflict(m),
            RadioError::Provider(p) => {
                let status = match &p {
                    ProviderError::NotFound(_) => StatusCode::NOT_FOUND,
                    ProviderError::Unsupported(_) => StatusCode::BAD_REQUEST,
                    ProviderError::RateLimited { .. } => StatusCode::SERVICE_UNAVAILABLE,
                    ProviderError::Network(_) | ProviderError::Parse(_) => StatusCode::BAD_GATEWAY,
                };
                ApiError::Validation {
                    status,
                    message: p.to_string(),
                }
            }
            other => ApiError::internal(other),
        }
    }
}

fn require_admin(user: &CurrentUser) -> ApiResult<()> {
    user.require(Permissions::ADMIN)
}

// ---------------- shows ----------------

/// Radio shows (search, station, followed, monitored; sortable, optionally paged).
#[utoipa::path(get, path = "/api/v1/radio/shows", tag = "radio", params(RadioShowListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Shows", body = Paged<RadioShowDto>),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
    ))]
pub async fn list_shows(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<RadioShowListQuery>,
) -> ApiResult<ApiJson<Paged<RadioShowDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.list_shows(&q).await?))
}

/// Adds a show from a search hit (`provider` + `external_id`) or a pasted `link`, with its
/// series and episodes.
#[utoipa::path(post, path = "/api/v1/radio/shows", tag = "radio", request_body = AddRadioShowRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Added", body = RadioShowDto),
        (status = 400, description = "Unrecognised link, invalid root folder or profile", body = ApiErrorBody),
        (status = 409, description = "Already in the library", body = ApiErrorBody),
        (status = 502, description = "The source failed", body = ApiErrorBody),
    ))]
pub async fn add_show(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AddRadioShowRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(created(svc(&state)?.add_show(req).await?))
}

/// One show.
#[utoipa::path(get, path = "/api/v1/radio/shows/{id}", tag = "radio",
    params(("id" = i64, Path, description = "Show id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Show", body = RadioShowDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_show(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<RadioShowDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_show(id).await?))
}

/// Edits a show (partial): follow choice, monitoring, tags, root folder, quality profile,
/// Audiobookshelf item.
#[utoipa::path(put, path = "/api/v1/radio/shows/{id}", tag = "radio", request_body = RadioShowEdit,
    params(("id" = i64, Path, description = "Show id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = RadioShowDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn edit_show(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(edit): ApiJson<RadioShowEdit>,
) -> ApiResult<ApiJson<RadioShowDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.update_show(id, edit).await?))
}

/// Deletes a show with its series and episodes (files in the podcast library are kept).
#[utoipa::path(delete, path = "/api/v1/radio/shows/{id}", tag = "radio",
    params(("id" = i64, Path, description = "Show id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_show(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_show(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A show's series and episodes.
#[utoipa::path(get, path = "/api/v1/radio/shows/{id}/episodes", tag = "radio",
    params(("id" = i64, Path, description = "Show id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Series and episodes", body = RadioShowEpisodesDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn show_episodes(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<RadioShowEpisodesDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.show_episodes(id).await?))
}

/// Queues a refresh of one show from its source.
#[utoipa::path(post, path = "/api/v1/radio/shows/{id}/refresh", tag = "radio",
    params(("id" = i64, Path, description = "Show id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn refresh_show(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let s = svc(&state)?;
    s.get_show(id).await?;
    Ok(accepted(s.queue_refresh(vec![id]).await?))
}

// ---------------- episodes ----------------

/// One episode with its matched library file.
#[utoipa::path(get, path = "/api/v1/radio/episodes/{id}", tag = "radio",
    params(("id" = i64, Path, description = "Episode id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Episode", body = RadioEpisodeDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_episode(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<RadioEpisodeDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_episode(id).await?))
}

/// Links a podcast library file to the episode by hand (kept by automatic matching).
#[utoipa::path(put, path = "/api/v1/radio/episodes/{id}/file", tag = "radio", request_body = RadioEpisodeFileRequest,
    params(("id" = i64, Path, description = "Episode id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Matched", body = RadioEpisodeDto),
        (status = 400, description = "No such file", body = ApiErrorBody),
        (status = 404, description = "No such episode", body = ApiErrorBody),
        (status = 409, description = "The file is matched by hand to another episode", body = ApiErrorBody),
    ))]
pub async fn set_episode_file(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<RadioEpisodeFileRequest>,
) -> ApiResult<ApiJson<RadioEpisodeDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.set_episode_file(id, req.file_id).await?))
}

/// Clears the episode's file match; it returns to Wanted or Not requested.
#[utoipa::path(delete, path = "/api/v1/radio/episodes/{id}/file", tag = "radio",
    params(("id" = i64, Path, description = "Episode id"), RadioClearFileQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Cleared", body = RadioEpisodeDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn clear_episode_file(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<RadioClearFileQuery>,
) -> ApiResult<ApiJson<RadioEpisodeDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.clear_episode_file(id, q.auto).await?))
}

// ---------------- search / preview / stations ----------------

/// Finds shows by name (one provider or every enabled one that can search); a pasted link
/// is previewed. Saves nothing.
#[utoipa::path(get, path = "/api/v1/radio/search", tag = "radio", params(RadioSearchQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Shows", body = Vec<RadioShowHitDto>),
        (status = 400, description = "Empty query, or no source can search", body = ApiErrorBody),
        (status = 502, description = "The source failed", body = ApiErrorBody),
    ))]
pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<RadioSearchQuery>,
) -> ApiResult<ApiJson<Vec<RadioShowHitDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.search(&q.q, q.provider.as_deref()).await?))
}

/// The show behind a link (show page or feed), with its newest episodes. Saves nothing.
#[utoipa::path(get, path = "/api/v1/radio/preview", tag = "radio", params(RadioPreviewQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Preview", body = RadioShowPreviewDto),
        (status = 400, description = "No source recognises the link", body = ApiErrorBody),
        (status = 502, description = "The source failed", body = ApiErrorBody),
    ))]
pub async fn preview(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<RadioPreviewQuery>,
) -> ApiResult<ApiJson<RadioShowPreviewDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.preview(&q.link).await?))
}

/// Stations of the saved shows.
#[utoipa::path(get, path = "/api/v1/radio/stations", tag = "radio",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Stations", body = Vec<RadioStationDto>)))]
pub async fn stations(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<RadioStationDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.stations().await?))
}

/// The radio sources and whether they can search and are enabled.
#[utoipa::path(get, path = "/api/v1/radio/providers", tag = "radio",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Sources", body = Vec<RadioProviderDto>)))]
pub async fn providers(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<RadioProviderDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.provider_list().await?))
}

/// Podcast library items no show covers, and files matched to no episode.
#[utoipa::path(get, path = "/api/v1/radio/library/review", tag = "radio",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Review", body = RadioLibraryReviewDto)))]
pub async fn library_review(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<RadioLibraryReviewDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.library_review().await?))
}

// ---------------- root folders / settings ----------------

/// Radio root folders (the podcast library folder as Reelix sees it) with free space.
#[utoipa::path(get, path = "/api/v1/radio/rootfolders", tag = "radio",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Root folders", body = Vec<ContentRootFolderDto>)))]
pub async fn list_root_folders(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<ContentRootFolderDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.root_folders().await?))
}

/// Adds a radio root folder (kind `radio`; must exist; may not overlap any root folder).
#[utoipa::path(post, path = "/api/v1/radio/rootfolders", tag = "radio", request_body = CreateContentRootFolderRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = ContentRootFolderDto),
        (status = 400, description = "Missing/relative path or wrong kind", body = ApiErrorBody),
        (status = 409, description = "Duplicate or overlapping", body = ApiErrorBody),
    ))]
pub async fn add_root_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<CreateContentRootFolderRequest>,
) -> ApiResult<Response> {
    require_admin(&user)?;
    Ok(created(svc(&state)?.add_root_folder(req).await?))
}

/// Removes a radio root folder (refused while shows use it).
#[utoipa::path(delete, path = "/api/v1/radio/rootfolders/{id}", tag = "radio",
    params(("id" = i64, Path, description = "Root folder id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 409, description = "In use", body = ApiErrorBody)))]
pub async fn delete_root_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    svc(&state)?.delete_root_folder(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Radio settings (podcast library, its folders, default follow choice, sources).
#[utoipa::path(get, path = "/api/v1/radio/settings", tag = "radio",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Settings", body = RadioSettingsDto), (status = 403, description = "Requires ADMIN", body = ApiErrorBody)))]
pub async fn get_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<RadioSettingsDto>> {
    require_admin(&user)?;
    Ok(ApiJson(svc(&state)?.settings().await?))
}

/// Validates (the folder must exist and be writable) and stores the radio settings.
#[utoipa::path(put, path = "/api/v1/radio/settings", tag = "radio", request_body = RadioSettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = RadioSettingsDto),
        (status = 400, description = "Folder missing or not writable, unknown source, bad code", body = ApiErrorBody),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn put_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<RadioSettingsDto>,
) -> ApiResult<ApiJson<RadioSettingsDto>> {
    require_admin(&user)?;
    Ok(ApiJson(svc(&state)?.set_settings(s).await?))
}
