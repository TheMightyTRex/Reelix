//! Discovery feeds, multi search and media details with availability (any authenticated user).

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use reelix_api_types::ApiErrorBody;
use reelix_api_types::requests::{DiscoverPageDto, GenreDto, MediaDetailDto, PageQuery, SearchPageDto};
use reelix_core::MediaKind;
use utoipa::OpenApi;

use super::requests::svc;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/discover/genres/{kind}", get(genres))
        .route("/discover/{*feed}", get(discover))
        .route("/search", get(search))
        .route("/media/movie/{tmdb_id}", get(movie))
        .route("/media/tv/{tmdb_id}", get(tv))
        .route("/media/{kind}/{tmdb_id}/recommendations", get(recommendations))
        .route("/media/{kind}/{tmdb_id}/similar", get(similar))
}

#[derive(OpenApi)]
#[openapi(
    paths(discover, genres, search, movie, tv, recommendations, similar),
    components(schemas(
        reelix_api_types::requests::DiscoverItemDto,
        reelix_api_types::requests::DiscoverPageDto,
        reelix_api_types::requests::RequestBriefDto,
        reelix_api_types::requests::PersonDto,
        reelix_api_types::requests::SearchPageDto,
        reelix_api_types::requests::GenreDto,
        reelix_api_types::requests::CompanyDto,
        reelix_api_types::requests::CastDto,
        reelix_api_types::requests::CrewDto,
        reelix_api_types::requests::VideoDto,
        reelix_api_types::requests::CollectionRefDto,
        reelix_api_types::requests::SeasonDto,
        reelix_api_types::requests::RequestAbilityDto,
        reelix_api_types::requests::MediaDetailDto,
        reelix_api_types::requests::PageQuery,
    )),
    tags((name = "discover", description = "Discovery feeds, search and media details with availability")),
)]
pub struct Api;

fn kind_of(s: &str) -> ApiResult<MediaKind> {
    match s {
        "movie" => Ok(MediaKind::Movie),
        "tv" | "series" => Ok(MediaKind::Series),
        other => Err(ApiError::not_found(format!("unknown media kind {other:?}"))),
    }
}

/// A discover feed: `trending`, `trending-week`, `trending-movies`, `trending-series`,
/// `popular-movies`, `popular-series`, `top-rated-movies`, `top-rated-series`, `upcoming`,
/// `now-playing`, `on-the-air`, `airing-today`, `genre/{movie|tv}/{id}`, `network/{id}`,
/// `studio/{id}`, `keyword/{movie|tv}/{id}`.
#[utoipa::path(get, path = "/api/v1/discover/{feed}", tag = "discover",
    params(("feed" = String, Path, description = "Feed path, may contain slashes"), PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Feed page", body = DiscoverPageDto),
        (status = 404, description = "Unknown feed", body = ApiErrorBody),
        (status = 502, description = "Metadata provider unavailable / not configured", body = ApiErrorBody),
    ))]
pub async fn discover(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(feed): ApiPath<String>,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<DiscoverPageDto>> {
    Ok(ApiJson(
        svc(&state)?
            .discover(&user.user, &feed, q.page.unwrap_or(1))
            .await?,
    ))
}

/// Genres for genre chips (`movie` or `tv`).
#[utoipa::path(get, path = "/api/v1/discover/genres/{kind}", tag = "discover",
    params(("kind" = String, Path, description = "`movie` or `tv`")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Genres", body = Vec<GenreDto>)))]
pub async fn genres(
    State(state): State<AppState>,
    _user: CurrentUser,
    ApiPath(kind): ApiPath<String>,
) -> ApiResult<ApiJson<Vec<GenreDto>>> {
    Ok(ApiJson(svc(&state)?.genres(kind_of(&kind)?).await?))
}

/// Movies, series and people matching `q`, with availability.
#[utoipa::path(get, path = "/api/v1/search", tag = "discover", params(PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Results", body = SearchPageDto)))]
pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<SearchPageDto>> {
    Ok(ApiJson(
        svc(&state)?
            .search(&user.user, q.q.as_deref().unwrap_or(""), q.page.unwrap_or(1))
            .await?,
    ))
}

/// Movie details with availability, requests and what the caller may request.
#[utoipa::path(get, path = "/api/v1/media/movie/{tmdb_id}", tag = "discover",
    params(("tmdb_id" = u32, Path, description = "TMDB id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Details", body = MediaDetailDto),
        (status = 404, description = "Unknown title", body = ApiErrorBody),
    ))]
pub async fn movie(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<u32>,
) -> ApiResult<ApiJson<MediaDetailDto>> {
    Ok(ApiJson(
        svc(&state)?
            .media_detail(&user.user, MediaKind::Movie, id)
            .await?,
    ))
}

/// Series details with per-season availability and request state.
#[utoipa::path(get, path = "/api/v1/media/tv/{tmdb_id}", tag = "discover",
    params(("tmdb_id" = u32, Path, description = "TMDB id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Details", body = MediaDetailDto),
        (status = 404, description = "Unknown title", body = ApiErrorBody),
    ))]
pub async fn tv(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<u32>,
) -> ApiResult<ApiJson<MediaDetailDto>> {
    Ok(ApiJson(
        svc(&state)?
            .media_detail(&user.user, MediaKind::Series, id)
            .await?,
    ))
}

/// Recommendations for a movie (`movie`) or series (`tv`).
#[utoipa::path(get, path = "/api/v1/media/{kind}/{tmdb_id}/recommendations", tag = "discover",
    params(("kind" = String, Path, description = "`movie` or `tv`"), ("tmdb_id" = u32, Path, description = "TMDB id"), PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Page", body = DiscoverPageDto)))]
pub async fn recommendations(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath((kind, id)): ApiPath<(String, u32)>,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<DiscoverPageDto>> {
    Ok(ApiJson(
        svc(&state)?
            .related(&user.user, kind_of(&kind)?, id, false, q.page.unwrap_or(1))
            .await?,
    ))
}

/// Similar titles for a movie (`movie`) or series (`tv`).
#[utoipa::path(get, path = "/api/v1/media/{kind}/{tmdb_id}/similar", tag = "discover",
    params(("kind" = String, Path, description = "`movie` or `tv`"), ("tmdb_id" = u32, Path, description = "TMDB id"), PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Page", body = DiscoverPageDto)))]
pub async fn similar(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath((kind, id)): ApiPath<(String, u32)>,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<DiscoverPageDto>> {
    Ok(ApiJson(
        svc(&state)?
            .related(&user.user, kind_of(&kind)?, id, true, q.page.unwrap_or(1))
            .await?,
    ))
}
