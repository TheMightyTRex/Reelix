//! Subtitles (fork issue #62): settings (admin), the subtitles fetched for a file and a search
//! now (Manage library). DTOs: `reelix_api_types::subtitles`.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use reelix_api_types::subtitles::{
    OpenSubtitlesSettings, SubtitleFileDto, SubtitleLanguage, SubtitleMediaKind, SubtitleSettings,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob};
use reelix_core::Permissions;
use reelix_subtitles::{Error as SubtitleError, SubtitleService};
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/subtitles", get(get_settings).put(put_settings))
        .route("/subtitles/{kind}/{file_id}", get(list))
        .route("/subtitles/{kind}/{file_id}/search", post(search))
}

#[derive(OpenApi)]
#[openapi(
    paths(get_settings, put_settings, list, search),
    components(schemas(
        SubtitleSettings,
        SubtitleLanguage,
        OpenSubtitlesSettings,
        SubtitleFileDto,
        SubtitleMediaKind,
    )),
    tags((name = "subtitles", description = "Subtitles from OpenSubtitles and Stremio subtitle add-ons, saved beside films and episodes"))
)]
pub struct Api;

impl From<SubtitleError> for ApiError {
    fn from(e: SubtitleError) -> Self {
        match e {
            SubtitleError::Settings(m) => ApiError::validation(m),
            SubtitleError::NotFound(m) => ApiError::not_found(m),
            SubtitleError::Off => ApiError::Conflict(SubtitleError::Off.to_string()),
            SubtitleError::Db(d) => d.into(),
            other => ApiError::internal(other),
        }
    }
}

fn svc(state: &AppState) -> ApiResult<std::sync::Arc<SubtitleService>> {
    state.ext::<SubtitleService>()
}

fn kind(s: &str) -> ApiResult<SubtitleMediaKind> {
    SubtitleMediaKind::parse(s).ok_or_else(|| ApiError::validation("kind must be movie or episode"))
}

/// Subtitle settings (admin). Off by default. The OpenSubtitles API key and password are masked.
#[utoipa::path(get, path = "/api/v1/settings/subtitles", tag = "subtitles",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Subtitle settings", body = SubtitleSettings),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn get_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<SubtitleSettings>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.settings_masked().await?))
}

/// Saves the subtitle settings (admin). Languages are two-letter codes, optionally with a
/// region (`pt-BR`); at most 10 languages and 5 add-ons. While `enabled`, at least one language
/// and one source are needed. Sending the masked API key or password keeps the stored one.
#[utoipa::path(put, path = "/api/v1/settings/subtitles", tag = "subtitles",
    request_body = SubtitleSettings,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = SubtitleSettings),
        (status = 400, description = "Not valid", body = ApiErrorBody),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn put_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(settings): ApiJson<SubtitleSettings>,
) -> ApiResult<ApiJson<SubtitleSettings>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_settings(settings).await?))
}

/// The subtitles Reelix fetched for a film's file (`movie`, a movie file id) or an episode file
/// (`episode`, an episode file id).
#[utoipa::path(get, path = "/api/v1/subtitles/{kind}/{file_id}", tag = "subtitles",
    params(
        ("kind" = String, Path, description = "`movie` or `episode`"),
        ("file_id" = i64, Path, description = "Movie file or episode file id"),
    ),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Fetched subtitles", body = Vec<SubtitleFileDto>),
        (status = 404, description = "No such file", body = ApiErrorBody),
    ))]
pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath((k, file_id)): ApiPath<(String, i64)>,
) -> ApiResult<ApiJson<Vec<SubtitleFileDto>>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    Ok(ApiJson(svc(&state)?.records(kind(&k)?, file_id).await?))
}

/// Searches for a file's subtitles now (job `subtitles_search`). 409 while subtitles are off.
#[utoipa::path(post, path = "/api/v1/subtitles/{kind}/{file_id}/search", tag = "subtitles",
    params(
        ("kind" = String, Path, description = "`movie` or `episode`"),
        ("file_id" = i64, Path, description = "Movie file or episode file id"),
    ),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued", body = EnqueuedJob),
        (status = 404, description = "No such file", body = ApiErrorBody),
        (status = 409, description = "Subtitles are off", body = ApiErrorBody),
    ))]
pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath((k, file_id)): ApiPath<(String, i64)>,
) -> ApiResult<Response> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    let job_id = svc(&state)?.queue_search(kind(&k)?, file_id).await?;
    Ok((StatusCode::ACCEPTED, ApiJson(EnqueuedJob { job_id })).into_response())
}
