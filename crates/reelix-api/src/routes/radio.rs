//! Radio: shows with their series and episodes, hand-set file matches, provider search and
//! preview, indexer searches (automatic and interactive), stations, the podcast library
//! review, radio root folders, radio settings and the naming of filed episodes.
//!
//! Reads and writes need `MANAGE_LIBRARY`, as for the other media types; radio settings and
//! root-folder changes need `ADMIN`. DTOs: `reelix_api_types::radio`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::content::{ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_api_types::library::Paged;
use reelix_api_types::pipeline::ReleaseDto;
use reelix_api_types::radio::{
    AddRadioShowRequest, RadioClearFileQuery, RadioEpisodeDto, RadioEpisodeFileRequest, RadioGenreDto,
    RadioLibraryReviewDto, RadioMonitorRequest, RadioNamingDto, RadioPreviewQuery, RadioProviderDto,
    RadioSearchQuery, RadioSearchRequest, RadioSettingsDto, RadioShowDto, RadioShowEdit,
    RadioShowEpisodesDto, RadioShowHitDto, RadioShowListQuery, RadioShowPreviewDto, RadioStationDto,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob};
use reelix_core::Permissions;
use reelix_pipeline::search::SearchService;
use reelix_radio::{ProviderError, RadioError, RadioService, StoredImage};
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
        .route("/radio/shows/{id}/artwork", get(show_artwork))
        .route("/radio/shows/{id}/refresh", post(refresh_show))
        .route("/radio/shows/{id}/search", post(search_show))
        .route("/radio/episodes/{id}", get(get_episode).put(edit_episode))
        .route("/radio/series/{id}", put(edit_series))
        .route("/radio/episodes/{id}/search", post(search_episode))
        .route("/radio/episodes/{id}/releases", get(episode_releases))
        .route(
            "/radio/episodes/{id}/file",
            put(set_episode_file).delete(clear_episode_file),
        )
        .route("/radio/search", get(search))
        .route("/radio/preview", get(preview))
        .route("/radio/stations", get(stations))
        .route("/radio/stations/{id}/logo", get(station_logo))
        .route("/radio/providers", get(providers))
        .route("/radio/genres", get(genres))
        .route("/radio/library/review", get(library_review))
        .route("/radio/rootfolders", get(list_root_folders).post(add_root_folder))
        .route("/radio/rootfolders/{id}", delete(delete_root_folder))
        .route("/radio/settings", get(get_settings).put(put_settings))
        .route("/radio/naming", get(get_naming).put(put_naming))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_shows, add_show, get_show, edit_show, delete_show, show_episodes, show_artwork, refresh_show,
        get_episode, set_episode_file, clear_episode_file, search, preview, stations, station_logo, providers, genres,
        library_review, list_root_folders, add_root_folder, delete_root_folder, get_settings, put_settings,
        search_show, search_episode, episode_releases, get_naming, put_naming, edit_episode, edit_series,
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
        reelix_api_types::radio::RadioProviderStationDto,
        reelix_api_types::radio::RadioGenreDto,
        reelix_api_types::radio::AddRadioShowRequest,
        reelix_api_types::radio::RadioShowEdit,
        reelix_api_types::radio::RadioEpisodeFileRequest,
        reelix_api_types::radio::RadioMonitorRequest,
        reelix_api_types::radio::RadioLibraryReviewDto,
        reelix_api_types::radio::RadioLibraryShowDto,
        reelix_api_types::radio::RadioSettingsDto,
        reelix_api_types::radio::RadioSearchRequest,
        reelix_api_types::radio::RadioNamingDto,
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

/// How long browsers may keep artwork and logos before asking again (with `If-None-Match`).
const IMAGE_MAX_AGE: &str = "private, max-age=3600";
/// Opened directly, an SVG still cannot run script or load anything.
const SVG_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// A downloaded image with its ETag; `304` when `If-None-Match` names it.
fn image_response(img: StoredImage, req: &HeaderMap) -> Response {
    let matches = req
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().trim_start_matches("W/"))
        .any(|t| t == img.etag || t == "*");
    let is_svg = img.is_svg();
    let mut res = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mut r = (StatusCode::OK, img.bytes).into_response();
        r.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(img.content_type));
        r
    };
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&img.etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(IMAGE_MAX_AGE));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if is_svg {
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(SVG_CSP));
    }
    res
}

/// The show's artwork, downloaded on refresh (JPEG, PNG, WebP or cleaned SVG). Supports
/// `If-None-Match`.
#[utoipa::path(get, path = "/api/v1/radio/shows/{id}/artwork", tag = "radio",
    params(("id" = i64, Path, description = "Show id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "The image (ETag, Cache-Control)", content_type = "image/*"),
        (status = 304, description = "Unchanged (If-None-Match)"),
        (status = 404, description = "No such show, or no artwork downloaded", body = ApiErrorBody),
    ))]
pub async fn show_artwork(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let img = svc(&state)?
        .show_artwork(id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("radio show {id} has no artwork")))?;
    Ok(image_response(img, &headers))
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

/// Queues an indexer search for the show's wanted episodes (job `radio_episode_search`).
#[utoipa::path(post, path = "/api/v1/radio/shows/{id}/search", tag = "radio",
    params(("id" = i64, Path, description = "Show id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued", body = EnqueuedJob),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn search_show(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let job = svc(&state)?
        .request_search(RadioSearchRequest {
            kind: ContentSearchKind::Parent,
            ids: vec![id],
        })
        .await?;
    Ok(accepted(job))
}

// ---------------- episodes ----------------

/// Queues an indexer search for one episode (job `radio_episode_search`); unmonitored episodes
/// can be searched by hand, episodes with a file are skipped.
#[utoipa::path(post, path = "/api/v1/radio/episodes/{id}/search", tag = "radio",
    params(("id" = i64, Path, description = "Episode id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued", body = EnqueuedJob),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn search_episode(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let job = svc(&state)?
        .request_search(RadioSearchRequest {
            kind: ContentSearchKind::Item,
            ids: vec![id],
        })
        .await?;
    Ok(accepted(job))
}

/// Interactive search for one episode: every release from interactive-enabled indexers in the
/// radio categories, with its decision, best first. Grab one with `POST /releases/grab`.
#[utoipa::path(get, path = "/api/v1/radio/episodes/{id}/releases", tag = "radio",
    params(("id" = i64, Path, description = "Episode id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Releases, best first", body = Vec<ReleaseDto>),
        (status = 400, description = "The episode already has a file", body = ApiErrorBody),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn episode_releases(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<Vec<ReleaseDto>>> {
    require_lib(&user)?;
    let search = state.ext::<SearchService>()?;
    Ok(ApiJson(search.interactive_radio(id).await?))
}

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

/// Monitors or unmonitors one episode by hand; its status follows (Not requested when
/// unmonitored, else Wanted, Not released or In library).
#[utoipa::path(put, path = "/api/v1/radio/episodes/{id}", tag = "radio", request_body = RadioMonitorRequest,
    params(("id" = i64, Path, description = "Episode id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = RadioEpisodeDto),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn edit_episode(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<RadioMonitorRequest>,
) -> ApiResult<ApiJson<RadioEpisodeDto>> {
    require_lib(&user)?;
    Ok(ApiJson(
        svc(&state)?.set_episode_monitored(id, req.monitored).await?,
    ))
}

/// Monitors or unmonitors every episode of a series by hand; returns the series' episodes.
#[utoipa::path(put, path = "/api/v1/radio/series/{id}", tag = "radio", request_body = RadioMonitorRequest,
    params(("id" = i64, Path, description = "Series id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "The series' episodes", body = Vec<RadioEpisodeDto>),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn edit_series(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<RadioMonitorRequest>,
) -> ApiResult<ApiJson<Vec<RadioEpisodeDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(
        svc(&state)?.set_series_monitored(id, req.monitored).await?,
    ))
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
    let mut hits = svc(&state)?.search(&q.q, q.provider.as_deref()).await?;
    if q.from_stations {
        hits.retain(|h| h.broadcaster.is_some());
    }
    Ok(ApiJson(hits))
}

/// The shared radio genres (RFX-93): one list for every source; `all-genre-{id}` is each one's Discover row.
#[utoipa::path(get, path = "/api/v1/radio/genres", tag = "radio",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Genres", body = Vec<RadioGenreDto>)))]
pub async fn genres(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> ApiResult<ApiJson<Vec<RadioGenreDto>>> {
    Ok(ApiJson(svc(&state)?.genres()))
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

/// The station's logo, downloaded on refresh (SVG logos are cleaned and served with a strict
/// content security policy). Supports `If-None-Match`.
#[utoipa::path(get, path = "/api/v1/radio/stations/{id}/logo", tag = "radio",
    params(("id" = i64, Path, description = "Station id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "The image (ETag, Cache-Control)", content_type = "image/*"),
        (status = 304, description = "Unchanged (If-None-Match)"),
        (status = 404, description = "No such station, or no logo downloaded", body = ApiErrorBody),
    ))]
pub async fn station_logo(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let img = svc(&state)?
        .station_logo(id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("radio station {id} has no logo")))?;
    Ok(image_response(img, &headers))
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

/// Radio settings (podcast library, its folders, default follow choice, sources). The Podcast
/// Index secret is masked.
#[utoipa::path(get, path = "/api/v1/radio/settings", tag = "radio",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Settings", body = RadioSettingsDto), (status = 403, description = "Requires ADMIN", body = ApiErrorBody)))]
pub async fn get_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<RadioSettingsDto>> {
    require_admin(&user)?;
    Ok(ApiJson(svc(&state)?.settings_masked().await?))
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

// ---------------- naming ----------------

/// Naming of filed episodes (show folder, episode file, tag writing).
#[utoipa::path(get, path = "/api/v1/radio/naming", tag = "radio",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Naming", body = RadioNamingDto),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn get_naming(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<RadioNamingDto>> {
    require_admin(&user)?;
    Ok(ApiJson(svc(&state)?.naming().await?))
}

/// Validates (non-empty, known tokens) and stores the naming of filed episodes.
#[utoipa::path(put, path = "/api/v1/radio/naming", tag = "radio", request_body = RadioNamingDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = RadioNamingDto),
        (status = 400, description = "Empty template or unknown token", body = ApiErrorBody),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn put_naming(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(n): ApiJson<RadioNamingDto>,
) -> ApiResult<ApiJson<RadioNamingDto>> {
    require_admin(&user)?;
    Ok(ApiJson(svc(&state)?.set_naming(n).await?))
}
