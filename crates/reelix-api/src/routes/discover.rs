//! Discovery feeds, multi search and media details with availability (any authenticated user).

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use reelix_api_types::ApiErrorBody;
use reelix_api_types::requests::{
    BookSearchPageDto, ContentDetailDto, ContentRequestKind, ContentRowDto, DiscoverPageDto, GenreDto,
    MediaDetailDto, MusicSearchPageDto, PageQuery, RadioDiscoverDto, RadioSearchPageDto, SearchPageDto,
    radio_foreign_id,
};
use reelix_core::MediaKind;
use utoipa::OpenApi;

use super::requests::svc;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/discover/genres/radio", get(radio_genres))
        .route("/discover/genres/{kind}", get(genres))
        .route("/discover/content/{kind}/{row}", get(discover_content))
        .route("/discover/{*feed}", get(discover))
        .route("/search", get(search))
        .route("/search/books", get(search_books))
        .route("/search/music", get(search_music))
        .route("/search/radio", get(search_radio))
        .route("/media/book/{work_id}", get(book))
        .route("/media/album/{release_group_id}", get(album))
        .route("/media/radio/{provider}/{external_id}", get(radio))
        .route("/media/movie/{tmdb_id}", get(movie))
        .route("/media/tv/{tmdb_id}", get(tv))
        .route("/media/{kind}/{tmdb_id}/recommendations", get(recommendations))
        .route("/media/{kind}/{tmdb_id}/similar", get(similar))
}

#[derive(OpenApi)]
#[openapi(
    paths(discover, discover_content, genres, radio_genres, search, movie, tv, recommendations, similar, search_books,
        search_music, search_radio, book, album, radio),
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
        reelix_api_types::requests::ContentRequestKind,
        reelix_api_types::requests::ContentRequestBriefDto,
        reelix_api_types::requests::ContentItemDto,
        reelix_api_types::requests::ArtistItemDto,
        reelix_api_types::requests::BookSearchPageDto,
        reelix_api_types::requests::MusicSearchPageDto,
        reelix_api_types::requests::ContentRequestAbilityDto,
        reelix_api_types::requests::ContentDetailDto,
        reelix_api_types::requests::ContentRowDto,
        reelix_api_types::requests::RadioSearchPageDto,
        reelix_api_types::requests::RadioDiscoverDto,
        reelix_api_types::requests::RadioDiscoverListDto,
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

/// Books (Open Library works) matching `q`, with availability and request state.
/// `isbn:…`, `olid:OL…W` and bare ISBNs look up a single work.
#[utoipa::path(get, path = "/api/v1/search/books", tag = "discover", params(PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Results", body = BookSearchPageDto),
        (status = 502, description = "Open Library unavailable", body = ApiErrorBody),
    ))]
pub async fn search_books(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<BookSearchPageDto>> {
    Ok(ApiJson(
        svc(&state)?
            .search_books(&user.user, q.q.as_deref().unwrap_or(""))
            .await?,
    ))
}

/// Albums (MusicBrainz release groups, with availability and request state) and artists
/// matching `q`.
#[utoipa::path(get, path = "/api/v1/search/music", tag = "discover", params(PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Results", body = MusicSearchPageDto),
        (status = 502, description = "MusicBrainz unavailable", body = ApiErrorBody),
    ))]
pub async fn search_music(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<MusicSearchPageDto>> {
    Ok(ApiJson(
        svc(&state)?
            .search_music(&user.user, q.q.as_deref().unwrap_or(""))
            .await?,
    ))
}

/// Radio shows from the enabled radio sources matching `q` (a pasted link is previewed),
/// with follow and request state.
#[utoipa::path(get, path = "/api/v1/search/radio", tag = "discover", params(PageQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Results", body = RadioSearchPageDto),
        (status = 502, description = "Radio sources unavailable", body = ApiErrorBody),
    ))]
pub async fn search_radio(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<PageQuery>,
) -> ApiResult<ApiJson<RadioSearchPageDto>> {
    Ok(ApiJson(
        svc(&state)?
            .search_radio(&user.user, q.q.as_deref().unwrap_or(""))
            .await?,
    ))
}

/// The radio Discover lists of the enabled sources for the radio settings' country and
/// language: rows (Apple's top podcasts, Podcast Index trending, popular on BBC Sounds) and
/// categories. Their ids are rows of `GET /discover/content/radio/{row}`.
#[utoipa::path(get, path = "/api/v1/discover/genres/radio", tag = "discover",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Rows and categories", body = RadioDiscoverDto)))]
pub async fn radio_genres(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> ApiResult<ApiJson<RadioDiscoverDto>> {
    Ok(ApiJson(svc(&state)?.radio_lists().await?))
}

/// One Discover row of books (Open Library trending and subjects), albums (ListenBrainz new
/// releases and charts) or radio shows (a list from `GET /discover/genres/radio`), with
/// availability and request state.
#[utoipa::path(get, path = "/api/v1/discover/content/{kind}/{row}", tag = "discover",
    params(
        ("kind" = String, Path, description = "`book`, `music` or `radio`"),
        ("row" = String, Path, description = "Row id, e.g. `trending`, `subject-fantasy`, `new-releases`, `popular-week`, `apple-top`"),
    ),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Row items", body = ContentRowDto),
        (status = 404, description = "Unknown kind or row", body = ApiErrorBody),
        (status = 502, description = "Open Library, ListenBrainz or the radio source unavailable", body = ApiErrorBody),
    ))]
pub async fn discover_content(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath((kind, row)): ApiPath<(String, String)>,
) -> ApiResult<ApiJson<ContentRowDto>> {
    let kind = ContentRequestKind::parse(&kind)
        .ok_or_else(|| ApiError::not_found(format!("unknown content kind {kind:?}")))?;
    Ok(ApiJson(
        svc(&state)?.discover_content(&user.user, kind, &row).await?,
    ))
}

/// A book's request view: availability, requests and the formats the caller may request.
#[utoipa::path(get, path = "/api/v1/media/book/{work_id}", tag = "discover",
    params(("work_id" = String, Path, description = "Open Library work id (`OL…W`)")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Details", body = ContentDetailDto),
        (status = 404, description = "Unknown work", body = ApiErrorBody),
    ))]
pub async fn book(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<ApiJson<ContentDetailDto>> {
    Ok(ApiJson(
        svc(&state)?
            .content_detail(&user.user, ContentRequestKind::Book, &id)
            .await?,
    ))
}

/// An album's request view: availability, requests and what the caller may request.
#[utoipa::path(get, path = "/api/v1/media/album/{release_group_id}", tag = "discover",
    params(("release_group_id" = String, Path, description = "MusicBrainz release-group id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Details", body = ContentDetailDto),
        (status = 404, description = "Unknown release group", body = ApiErrorBody),
    ))]
pub async fn album(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<ApiJson<ContentDetailDto>> {
    Ok(ApiJson(
        svc(&state)?
            .content_detail(&user.user, ContentRequestKind::Music, &id)
            .await?,
    ))
}

/// A radio show's request view: its source's data with the newest episodes, follow state,
/// requests and whether the caller may request it. `external_id` is percent-encoded (a feed
/// URL for `rss`).
#[utoipa::path(get, path = "/api/v1/media/radio/{provider}/{external_id}", tag = "discover",
    params(
        ("provider" = String, Path, description = "Radio source id (`bbc`, `apple`, `podcastindex`, `archive`, `rss`)"),
        ("external_id" = String, Path, description = "The show's id at the source"),
    ),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Details", body = ContentDetailDto),
        (status = 404, description = "Unknown show or source", body = ApiErrorBody),
        (status = 502, description = "Radio source unavailable", body = ApiErrorBody),
    ))]
pub async fn radio(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath((provider, external_id)): ApiPath<(String, String)>,
) -> ApiResult<ApiJson<ContentDetailDto>> {
    Ok(ApiJson(
        svc(&state)?
            .content_detail(
                &user.user,
                ContentRequestKind::Radio,
                &radio_foreign_id(&provider, &external_id),
            )
            .await?,
    ))
}
