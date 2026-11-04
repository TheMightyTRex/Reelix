//! Movie & series library (M2/M5): movies, collections, root folders, library import, search
//! hand-off and statistics. Series/episodes live in `library_series.rs`; calendar, wanted,
//! rename and settings in `library_misc.rs`.
//!
//! Reads and writes need `MANAGE_LIBRARY`; settings need `ADMIN`; the calendar also accepts
//! `VIEW_CALENDAR`. DTOs: `reelix_api_types::library`.

#[path = "library_misc.rs"]
mod misc;
#[path = "library_series.rs"]
mod series;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use reelix_api_types::library::{
    AddMovieRequest, BulkDeleteRequest, CollectionDto, CollectionEdit, CreateRootFolderRequest, DeleteQuery,
    ImportCandidateDto, LibraryImportMatchRequest, LibraryImportRequest, LibraryImportResult,
    LibraryImportScanQuery, LibraryStats, LookupQuery, LookupResult, MovieBulkEdit, MovieDto, MovieEdit,
    MovieFileDto, MovieListQuery, Paged, RootFolderDto, SearchRequest,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob};
use reelix_core::Permissions;
use reelix_library::{LibraryError, LibraryService};
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/movies", get(list_movies).post(add_movie))
        .route("/movies/bulk", put(bulk_edit_movies).delete(bulk_delete_movies))
        .route("/movies/lookup", get(lookup_movies))
        .route(
            "/movies/{id}",
            get(get_movie).put(edit_movie).delete(delete_movie),
        )
        .route("/movies/{id}/files", get(movie_files))
        .route("/movies/{id}/refresh", post(refresh_movie))
        .route("/movies/{id}/rescan", post(rescan_movie))
        .route("/moviefiles/{id}", axum::routing::delete(delete_movie_file))
        .route("/collections", get(list_collections))
        .route("/collections/{id}", put(edit_collection))
        .route("/rootfolders", get(list_root_folders).post(add_root_folder))
        .route("/rootfolders/{id}", axum::routing::delete(delete_root_folder))
        .route("/libraryimport/scan", get(import_scan))
        .route("/libraryimport/match", post(import_match))
        .route("/libraryimport", post(library_import))
        .route("/search", post(search))
        .route("/library/stats", get(stats))
        .merge(series::routes())
        .merge(misc::routes())
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_movies, add_movie, get_movie, edit_movie, delete_movie, bulk_edit_movies, bulk_delete_movies,
        lookup_movies, movie_files, delete_movie_file, refresh_movie, rescan_movie,
        list_collections, edit_collection, list_root_folders, add_root_folder, delete_root_folder,
        import_scan, import_match, library_import, search, stats,
    ),
    components(schemas(
        reelix_api_types::library::MovieDto,
        reelix_api_types::library::MovieFileDto,
        reelix_api_types::library::MediaFileDto,
        reelix_api_types::library::MediaInfoDto,
        reelix_api_types::library::ImagesDto,
        reelix_api_types::library::RatingsDto,
        reelix_api_types::library::CastDto,
        reelix_api_types::library::CollectionRefDto,
        reelix_api_types::library::MovieStatus,
        reelix_api_types::library::MinimumAvailability,
        reelix_api_types::library::MovieMonitor,
        reelix_api_types::library::TagMode,
        reelix_api_types::library::SortDir,
        reelix_api_types::library::AddMovieRequest,
        reelix_api_types::library::MovieEdit,
        reelix_api_types::library::MovieBulkEdit,
        reelix_api_types::library::BulkDeleteRequest,
        reelix_api_types::library::LookupResult,
        reelix_api_types::library::CollectionDto,
        reelix_api_types::library::CollectionMovieDto,
        reelix_api_types::library::CollectionEdit,
        reelix_api_types::library::RootFolderDto,
        reelix_api_types::library::UnmappedFolderDto,
        reelix_api_types::library::CreateRootFolderRequest,
        reelix_api_types::library::ImportCandidateDto,
        reelix_api_types::library::LibraryImportItem,
        reelix_api_types::library::LibraryImportRequest,
        reelix_api_types::library::LibraryImportResult,
        reelix_api_types::library::ImportErrorDto,
        reelix_api_types::library::SearchRequest,
        reelix_api_types::library::LibrarySearchKind,
        reelix_api_types::library::LibraryStats,
        reelix_api_types::library::MovieStats,
        reelix_api_types::library::SeriesStats,
        reelix_api_types::library::RootFolderSpaceDto,
        reelix_api_types::library::RecentItemDto,
    )),
    tags((name = "library", description = "Movies, series, episodes, files, collections, root folders, calendar, wanted and library settings")),
)]
struct MoviesApi;

/// OpenAPI for every library route (movies + series + misc).
pub struct Api;

impl OpenApi for Api {
    fn openapi() -> utoipa::openapi::OpenApi {
        let mut doc = MoviesApi::openapi();
        doc.merge(series::Api::openapi());
        doc.merge(misc::Api::openapi());
        doc
    }
}

impl From<LibraryError> for ApiError {
    fn from(e: LibraryError) -> Self {
        match e {
            LibraryError::NotFound(m) => ApiError::not_found(m),
            LibraryError::Validation(m) => ApiError::validation(m),
            LibraryError::Conflict(m) => ApiError::Conflict(m),
            LibraryError::Metadata(reelix_metadata_error) => {
                let msg = reelix_metadata_error.to_string();
                if msg.contains("not configured") {
                    ApiError::Validation {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        message: msg,
                    }
                } else if msg == "not found" {
                    ApiError::not_found("not found on TMDB")
                } else {
                    ApiError::Validation {
                        status: StatusCode::BAD_GATEWAY,
                        message: format!("metadata provider: {msg}"),
                    }
                }
            }
            LibraryError::Files(f) => ApiError::validation(f.to_string()),
            other => ApiError::internal(other),
        }
    }
}

pub(crate) fn svc(state: &AppState) -> ApiResult<Arc<LibraryService>> {
    state.ext::<LibraryService>()
}

pub(crate) fn require_lib(user: &CurrentUser) -> ApiResult<()> {
    user.require(Permissions::MANAGE_LIBRARY)
}

pub(crate) fn created<T: serde::Serialize>(v: T) -> Response {
    (StatusCode::CREATED, ApiJson(v)).into_response()
}

pub(crate) fn accepted(job_id: Option<i64>) -> Response {
    (StatusCode::ACCEPTED, ApiJson(EnqueuedJob { job_id })).into_response()
}

// ---------------- movies ----------------

/// Movies (filterable, sortable, optionally paged).
#[utoipa::path(get, path = "/api/v1/movies", tag = "library", params(MovieListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Movies", body = Paged<MovieDto>),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
    ))]
pub async fn list_movies(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<MovieListQuery>,
) -> ApiResult<ApiJson<Paged<MovieDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.list_movies(&q).await?))
}

/// Adds a movie from TMDB.
#[utoipa::path(post, path = "/api/v1/movies", tag = "library", request_body = AddMovieRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Added", body = MovieDto),
        (status = 400, description = "Invalid profile/root folder", body = ApiErrorBody),
        (status = 409, description = "Already in the library", body = ApiErrorBody),
        (status = 503, description = "TMDB not configured", body = ApiErrorBody),
    ))]
pub async fn add_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AddMovieRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(created(svc(&state)?.add_movie(req).await?))
}

/// One movie (with cast).
#[utoipa::path(get, path = "/api/v1/movies/{id}", tag = "library",
    params(("id" = i64, Path, description = "Movie id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Movie", body = MovieDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<MovieDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_movie(id).await?))
}

/// Edits a movie (partial).
#[utoipa::path(put, path = "/api/v1/movies/{id}", tag = "library", request_body = MovieEdit,
    params(("id" = i64, Path, description = "Movie id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = MovieDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn edit_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(edit): ApiJson<MovieEdit>,
) -> ApiResult<ApiJson<MovieDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.update_movie(id, edit).await?))
}

/// Deletes a movie (`?delete_files=true` also removes its folder).
#[utoipa::path(delete, path = "/api/v1/movies/{id}", tag = "library",
    params(("id" = i64, Path, description = "Movie id"), DeleteQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<DeleteQuery>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_movie(id, q.delete_files).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Bulk edit (profile, root folder (+move), monitored, availability, tags).
#[utoipa::path(put, path = "/api/v1/movies/bulk", tag = "library", request_body = MovieBulkEdit,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated movies", body = Vec<MovieDto>)))]
pub async fn bulk_edit_movies(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<MovieBulkEdit>,
) -> ApiResult<ApiJson<Vec<MovieDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.bulk_update_movies(req).await?))
}

/// Bulk delete.
#[utoipa::path(delete, path = "/api/v1/movies/bulk", tag = "library", request_body = BulkDeleteRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted")))]
pub async fn bulk_delete_movies(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<BulkDeleteRequest>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.bulk_delete_movies(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// TMDB movie search with in-library flags (`tmdb:603`, `imdb:tt0133093` supported).
#[utoipa::path(get, path = "/api/v1/movies/lookup", tag = "library", params(LookupQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Results", body = Vec<LookupResult>),
        (status = 503, description = "TMDB not configured", body = ApiErrorBody),
    ))]
pub async fn lookup_movies(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<LookupQuery>,
) -> ApiResult<ApiJson<Vec<LookupResult>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.lookup_movies(&q.term).await?))
}

/// Files of a movie.
#[utoipa::path(get, path = "/api/v1/movies/{id}/files", tag = "library",
    params(("id" = i64, Path, description = "Movie id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Files", body = Vec<MovieFileDto>)))]
pub async fn movie_files(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<Vec<MovieFileDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.movie_files(id).await?))
}

/// Deletes a movie file (to the recycle bin when configured).
#[utoipa::path(delete, path = "/api/v1/moviefiles/{id}", tag = "library",
    params(("id" = i64, Path, description = "Movie file id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_movie_file(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_movie_file(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Queues a metadata refresh of one movie.
#[utoipa::path(post, path = "/api/v1/movies/{id}/refresh", tag = "library",
    params(("id" = i64, Path, description = "Movie id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob)))]
pub async fn refresh_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_refresh_movie(id).await?))
}

/// Queues a disk scan of one movie.
#[utoipa::path(post, path = "/api/v1/movies/{id}/rescan", tag = "library",
    params(("id" = i64, Path, description = "Movie id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob)))]
pub async fn rescan_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_rescan_movie(id).await?))
}

// ---------------- collections ----------------

/// Movie collections with their parts and library state.
#[utoipa::path(get, path = "/api/v1/collections", tag = "library",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Collections", body = Vec<CollectionDto>)))]
pub async fn list_collections(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<CollectionDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.list_collections().await?))
}

/// Edits a collection; monitoring it queues a sync that adds its missing movies.
#[utoipa::path(put, path = "/api/v1/collections/{id}", tag = "library", request_body = CollectionEdit,
    params(("id" = i64, Path, description = "Collection id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = CollectionDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn edit_collection(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(edit): ApiJson<CollectionEdit>,
) -> ApiResult<ApiJson<CollectionDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.update_collection(id, edit).await?))
}

// ---------------- root folders ----------------

/// Root folders with free space and unmapped folders.
#[utoipa::path(get, path = "/api/v1/rootfolders", tag = "library",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Root folders", body = Vec<RootFolderDto>)))]
pub async fn list_root_folders(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<RootFolderDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.root_folders().await?))
}

/// Adds a root folder (must exist; may not overlap another).
#[utoipa::path(post, path = "/api/v1/rootfolders", tag = "library", request_body = CreateRootFolderRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = RootFolderDto),
        (status = 400, description = "Missing/relative path", body = ApiErrorBody),
        (status = 409, description = "Duplicate or overlapping", body = ApiErrorBody),
    ))]
pub async fn add_root_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<CreateRootFolderRequest>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    Ok(created(
        svc(&state)?.add_root_folder(&req.path, req.media_kind).await?,
    ))
}

/// Removes a root folder (refused while in use).
#[utoipa::path(delete, path = "/api/v1/rootfolders/{id}", tag = "library",
    params(("id" = i64, Path, description = "Root folder id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 409, description = "In use", body = ApiErrorBody)))]
pub async fn delete_root_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    svc(&state)?.delete_root_folder(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- library import ----------------

/// Unmapped folders of a root folder with TMDB suggestions.
#[utoipa::path(get, path = "/api/v1/libraryimport/scan", tag = "library", params(LibraryImportScanQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Candidates", body = Vec<ImportCandidateDto>)))]
pub async fn import_scan(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<LibraryImportScanQuery>,
) -> ApiResult<ApiJson<Vec<ImportCandidateDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(
        svc(&state)?
            .import_scan(q.root_folder_id, q.lookup.unwrap_or(true))
            .await?,
    ))
}

/// TMDB suggestions for up to 25 folders from a scan (`lookup=false`), so large libraries are
/// matched in short requests.
#[utoipa::path(post, path = "/api/v1/libraryimport/match", tag = "library", request_body = LibraryImportMatchRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Candidates with suggestions", body = Vec<ImportCandidateDto>),
        (status = 400, description = "Too many folders or a folder outside the root folder", body = ApiErrorBody),
    ))]
pub async fn import_match(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<LibraryImportMatchRequest>,
) -> ApiResult<ApiJson<Vec<ImportCandidateDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(
        svc(&state)?.import_match(req.root_folder_id, req.paths).await?,
    ))
}

/// Bulk-adds existing folders as movies/series (each gets a disk scan).
#[utoipa::path(post, path = "/api/v1/libraryimport", tag = "library", request_body = LibraryImportRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Result", body = LibraryImportResult)))]
pub async fn library_import(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<LibraryImportRequest>,
) -> ApiResult<ApiJson<LibraryImportResult>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.library_import(req).await?))
}

// ---------------- search hand-off & stats ----------------

/// Queues a search (job kind `search`, executed by the pipeline).
#[utoipa::path(post, path = "/api/v1/search", tag = "library", request_body = SearchRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued", body = EnqueuedJob),
        (status = 400, description = "Invalid request", body = ApiErrorBody),
    ))]
pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<SearchRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.request_search(req).await?))
}

/// Library counts, sizes, free space and recently added items.
#[utoipa::path(get, path = "/api/v1/library/stats", tag = "library",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Statistics", body = LibraryStats)))]
pub async fn stats(State(state): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<LibraryStats>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.stats().await?))
}
