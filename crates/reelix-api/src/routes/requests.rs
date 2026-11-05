//! Requests, quotas, issues, request settings and per-user notification preferences (M6).
//!
//! Requesters see only their own requests/issues unless they have `VIEW_REQUESTS` /
//! `MANAGE_ISSUES`. Approve/decline/retry need `MANAGE_REQUESTS`; settings need `ADMIN`.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use reelix_api_types::ApiErrorBody;
use reelix_api_types::requests::{
    ContentRequestDto, ContentRequestOptionsQuery, ContentRequestPageDto, CreateContentRequestDto,
    UpdateContentRequestDto,
};
use reelix_api_types::requests::{
    CreateCommentDto, CreateIssueDto, CreateRequestDto, DeclineRequestDto, IssueCountsDto, IssueDto,
    IssueListQuery, IssuePageDto, IssueStatus, NotificationPrefsDto, QuotaDto, RequestCountsDto, RequestDto,
    RequestListQuery, RequestOptionsDto, RequestOptionsQuery, RequestPageDto, RequestSettingsDto,
    UpdateIssueDto, UpdateRequestDto,
};
use reelix_core::{ContentKind, MediaKind, Permissions};
use reelix_requests::RequestService;
use std::sync::Arc;
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/requests", get(list_requests).post(create_request))
        .route("/requests/count", get(request_counts))
        .route("/requests/options", get(request_options))
        .route(
            "/requests/{id}",
            get(get_request).put(update_request).delete(delete_request),
        )
        .route("/requests/{id}/approve", post(approve_request))
        .route("/requests/{id}/decline", post(decline_request))
        .route("/requests/{id}/retry", post(retry_request))
        .route(
            "/requests/media",
            get(list_media_requests).post(create_media_request),
        )
        .route("/requests/media/options", get(media_request_options))
        .route(
            "/requests/media/{id}",
            get(get_media_request)
                .put(update_media_request)
                .delete(delete_media_request),
        )
        .route("/requests/media/{id}/approve", post(approve_media_request))
        .route("/requests/media/{id}/decline", post(decline_media_request))
        .route("/requests/media/{id}/retry", post(retry_media_request))
        .route("/users/{id}/quota", get(user_quota))
        .route("/issues", get(list_issues).post(create_issue))
        .route("/issues/count", get(issue_counts))
        .route(
            "/issues/{id}",
            get(get_issue).put(update_issue).delete(delete_issue),
        )
        .route("/issues/{id}/comments", post(add_comment))
        .route("/issues/{id}/resolve", post(resolve_issue))
        .route("/issues/{id}/reopen", post(reopen_issue))
        .route("/settings/requests", get(get_settings).put(put_settings))
        .route("/profile/notifications", get(get_prefs).put(put_prefs))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_requests, create_request, request_counts, request_options, get_request, update_request,
        delete_request, approve_request, decline_request, retry_request, user_quota,
        list_media_requests, create_media_request, media_request_options, get_media_request,
        update_media_request, delete_media_request, approve_media_request, decline_media_request,
        retry_media_request,
        list_issues, create_issue, issue_counts, get_issue, update_issue, delete_issue, add_comment,
        resolve_issue, reopen_issue, get_settings, put_settings, get_prefs, put_prefs,
    ),
    components(schemas(
        reelix_api_types::requests::AvailabilityStatus,
        reelix_api_types::requests::RequestStatus,
        reelix_api_types::requests::UserBriefDto,
        reelix_api_types::requests::NamedIdDto,
        reelix_api_types::requests::RequestDto,
        reelix_api_types::requests::CreateRequestDto,
        reelix_api_types::requests::UpdateRequestDto,
        reelix_api_types::requests::DeclineRequestDto,
        reelix_api_types::requests::RequestPageDto,
        reelix_api_types::requests::RequestCountsDto,
        reelix_api_types::requests::RequestOptionsDto,
        reelix_api_types::requests::RootFolderOptionDto,
        reelix_api_types::requests::QuotaDto,
        reelix_api_types::requests::QuotaUsageDto,
        reelix_api_types::requests::IssueType,
        reelix_api_types::requests::IssueStatus,
        reelix_api_types::requests::IssueDto,
        reelix_api_types::requests::IssueCommentDto,
        reelix_api_types::requests::CreateIssueDto,
        reelix_api_types::requests::UpdateIssueDto,
        reelix_api_types::requests::CreateCommentDto,
        reelix_api_types::requests::IssuePageDto,
        reelix_api_types::requests::IssueCountsDto,
        reelix_api_types::requests::RequestSettingsDto,
        reelix_api_types::requests::RequestDefaultsDto,
        reelix_api_types::requests::NotificationPrefsDto,
        reelix_api_types::requests::ContentRequestKind,
        reelix_api_types::requests::ContentRequestDto,
        reelix_api_types::requests::CreateContentRequestDto,
        reelix_api_types::requests::UpdateContentRequestDto,
        reelix_api_types::requests::ContentRequestPageDto,
    )),
    tags((name = "requests", description = "Media requests, approvals, quotas and issues")),
)]
pub struct Api;

impl From<reelix_requests::Error> for ApiError {
    fn from(e: reelix_requests::Error) -> Self {
        use reelix_requests::Error as E;
        let with = |status, message| ApiError::Validation { status, message };
        match e {
            E::NotFound(m) => ApiError::NotFound(m),
            E::Forbidden(m) => with(StatusCode::FORBIDDEN, m),
            E::Validation(m) => ApiError::validation(m),
            E::Conflict(m) => ApiError::Conflict(m),
            E::Unauthorized(m) => with(StatusCode::UNAUTHORIZED, m),
            // "Not configured" is a setup problem, not an upstream failure (matches the library's 503).
            E::Upstream(m) if m.contains("not configured") => with(StatusCode::SERVICE_UNAVAILABLE, m),
            E::Upstream(m) => with(StatusCode::BAD_GATEWAY, m),
            E::Db(d) => d.into(),
            E::Other(o) => ApiError::internal(o),
        }
    }
}

/// The registered request service.
pub(crate) fn svc(state: &AppState) -> ApiResult<Arc<RequestService>> {
    state.ext::<RequestService>()
}

fn created<T: serde::Serialize>(v: T) -> Response {
    (StatusCode::CREATED, ApiJson(v)).into_response()
}

// ---------------------------------------------------------------- requests

/// Requests (own, or all with VIEW_REQUESTS), paged.
#[utoipa::path(get, path = "/api/v1/requests", tag = "requests", params(RequestListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Page of requests", body = RequestPageDto),
        (status = 400, description = "Bad filter", body = ApiErrorBody),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
    ))]
pub async fn list_requests(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<RequestListQuery>,
) -> ApiResult<ApiJson<RequestPageDto>> {
    Ok(ApiJson(svc(&state)?.list_requests(&user.user, &q).await?))
}

/// Creates a request (permission, duplicate, availability and quota checks; may auto-approve).
#[utoipa::path(post, path = "/api/v1/requests", tag = "requests", request_body = CreateRequestDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = RequestDto),
        (status = 403, description = "Not permitted or quota exceeded", body = ApiErrorBody),
        (status = 409, description = "Already available or requested", body = ApiErrorBody),
        (status = 502, description = "Metadata / library unavailable", body = ApiErrorBody),
    ))]
pub async fn create_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(dto): ApiJson<CreateRequestDto>,
) -> ApiResult<Response> {
    Ok(created(svc(&state)?.create_request(&user.user, dto).await?))
}

/// Request counts per status and kind (scoped like the list).
#[utoipa::path(get, path = "/api/v1/requests/count", tag = "requests",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Counts", body = RequestCountsDto)))]
pub async fn request_counts(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<RequestCountsDto>> {
    Ok(ApiJson(svc(&state)?.request_counts(&user.user).await?))
}

/// Profiles and root folders for the advanced request options (REQUEST_ADVANCED).
#[utoipa::path(get, path = "/api/v1/requests/options", tag = "requests", params(RequestOptionsQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Options", body = RequestOptionsDto),
        (status = 403, description = "Requires REQUEST_ADVANCED", body = ApiErrorBody),
    ))]
pub async fn request_options(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<RequestOptionsQuery>,
) -> ApiResult<ApiJson<RequestOptionsDto>> {
    let kind = match q.kind.as_deref() {
        None | Some("movie") => MediaKind::Movie,
        Some("series" | "tv") => MediaKind::Series,
        Some(other) => return Err(ApiError::validation(format!("unknown kind {other:?}"))),
    };
    Ok(ApiJson(
        svc(&state)?
            .request_options(&user.user, kind, q.is_4k.unwrap_or(false))
            .await?,
    ))
}

/// One request.
#[utoipa::path(get, path = "/api/v1/requests/{id}", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Request", body = RequestDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<RequestDto>> {
    Ok(ApiJson(svc(&state)?.get_request(&user.user, id).await?))
}

/// Edits seasons / 4K / advanced options (owner while pending, or MANAGE_REQUESTS).
#[utoipa::path(put, path = "/api/v1/requests/{id}", tag = "requests", request_body = UpdateRequestDto,
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = RequestDto),
        (status = 403, description = "Not permitted", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn update_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(dto): ApiJson<UpdateRequestDto>,
) -> ApiResult<ApiJson<RequestDto>> {
    Ok(ApiJson(svc(&state)?.update_request(&user.user, id, dto).await?))
}

/// Deletes a request (owner while pending/declined/failed, or MANAGE_REQUESTS).
#[utoipa::path(delete, path = "/api/v1/requests/{id}", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Not permitted", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    svc(&state)?.delete_request(&user.user, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Approves a request: adds it to the library (defaults/overrides) and starts a search.
/// If adding fails the request comes back with status `failed` and a `failure_reason`.
#[utoipa::path(post, path = "/api/v1/requests/{id}/approve", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Approved (or failed)", body = RequestDto),
        (status = 403, description = "Requires MANAGE_REQUESTS", body = ApiErrorBody),
        (status = 409, description = "Not pending", body = ApiErrorBody),
    ))]
pub async fn approve_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<RequestDto>> {
    Ok(ApiJson(svc(&state)?.approve_request(&user.user, id).await?))
}

/// Declines a request with an optional reason.
#[utoipa::path(post, path = "/api/v1/requests/{id}/decline", tag = "requests", request_body = DeclineRequestDto,
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Declined", body = RequestDto),
        (status = 403, description = "Requires MANAGE_REQUESTS", body = ApiErrorBody),
        (status = 409, description = "Not pending", body = ApiErrorBody),
    ))]
pub async fn decline_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    body: axum::body::Bytes,
) -> ApiResult<ApiJson<RequestDto>> {
    let reason = decline_reason(&body)?;
    Ok(ApiJson(
        svc(&state)?.decline_request(&user.user, id, reason).await?,
    ))
}

/// Retries a failed request.
#[utoipa::path(post, path = "/api/v1/requests/{id}/retry", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Retried", body = RequestDto),
        (status = 409, description = "Not failed", body = ApiErrorBody),
    ))]
pub async fn retry_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<RequestDto>> {
    Ok(ApiJson(svc(&state)?.retry_request(&user.user, id).await?))
}

/// Quota usage of a user (own, or any with MANAGE_USERS / VIEW_REQUESTS).
#[utoipa::path(get, path = "/api/v1/users/{id}/quota", tag = "requests",
    params(("id" = i64, Path, description = "User id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Quota", body = QuotaDto),
        (status = 403, description = "Not your quota", body = ApiErrorBody),
    ))]
pub async fn user_quota(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<QuotaDto>> {
    Ok(ApiJson(svc(&state)?.quota(&user.user, id).await?))
}

// ---------------------------------------------------------------- book / album requests

/// Book, album and radio requests (own, or all with VIEW_REQUESTS), paged. `kind`: `book`,
/// `music`, `radio` or `all`; the other filters work like `GET /requests`.
#[utoipa::path(get, path = "/api/v1/requests/media", tag = "requests", params(RequestListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Page of requests", body = ContentRequestPageDto),
        (status = 400, description = "Bad filter", body = ApiErrorBody),
    ))]
pub async fn list_media_requests(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<RequestListQuery>,
) -> ApiResult<ApiJson<ContentRequestPageDto>> {
    Ok(ApiJson(svc(&state)?.list_content_requests(&user.user, &q).await?))
}

/// Requests a book (REQUEST_BOOK; choose `formats`), an album (REQUEST_MUSIC) or a radio show
/// to follow (REQUEST_RADIO; `foreign_id` = `{provider}:{external_id}`). Duplicate,
/// availability and quota checks; auto-approved with AUTO_APPROVE_BOOK / AUTO_APPROVE_MUSIC /
/// AUTO_APPROVE_RADIO or the auto-approve request settings.
#[utoipa::path(post, path = "/api/v1/requests/media", tag = "requests", request_body = CreateContentRequestDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = ContentRequestDto),
        (status = 403, description = "Not permitted or quota exceeded", body = ApiErrorBody),
        (status = 404, description = "Unknown work / release group / show", body = ApiErrorBody),
        (status = 409, description = "Already available or requested", body = ApiErrorBody),
        (status = 502, description = "Metadata / library unavailable", body = ApiErrorBody),
    ))]
pub async fn create_media_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(dto): ApiJson<CreateContentRequestDto>,
) -> ApiResult<Response> {
    Ok(created(
        svc(&state)?.create_content_request(&user.user, dto).await?,
    ))
}

/// Profiles and root folders for advanced book/album/radio requests (REQUEST_ADVANCED).
#[utoipa::path(get, path = "/api/v1/requests/media/options", tag = "requests",
    params(ContentRequestOptionsQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Options", body = RequestOptionsDto),
        (status = 403, description = "Requires REQUEST_ADVANCED", body = ApiErrorBody),
    ))]
pub async fn media_request_options(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<ContentRequestOptionsQuery>,
) -> ApiResult<ApiJson<RequestOptionsDto>> {
    let kind = match q.kind.as_deref() {
        None | Some("ebook" | "book") => ContentKind::Ebook,
        Some("audiobook") => ContentKind::Audiobook,
        Some("music" | "album") => ContentKind::Music,
        Some("radio") => ContentKind::Radio,
        Some(other) => return Err(ApiError::validation(format!("unknown kind {other:?}"))),
    };
    Ok(ApiJson(
        svc(&state)?.content_request_options(&user.user, kind).await?,
    ))
}

/// One book/album request.
#[utoipa::path(get, path = "/api/v1/requests/media/{id}", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Request", body = ContentRequestDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_media_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<ContentRequestDto>> {
    Ok(ApiJson(svc(&state)?.get_content_request(&user.user, id).await?))
}

/// Edits formats / advanced options (owner while pending, or MANAGE_REQUESTS).
#[utoipa::path(put, path = "/api/v1/requests/media/{id}", tag = "requests", request_body = UpdateContentRequestDto,
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = ContentRequestDto),
        (status = 403, description = "Not permitted", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn update_media_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(dto): ApiJson<UpdateContentRequestDto>,
) -> ApiResult<ApiJson<ContentRequestDto>> {
    Ok(ApiJson(
        svc(&state)?.update_content_request(&user.user, id, dto).await?,
    ))
}

/// Deletes a book/album request (owner while pending/declined/failed, or MANAGE_REQUESTS).
#[utoipa::path(delete, path = "/api/v1/requests/media/{id}", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Not permitted", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_media_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    svc(&state)?.delete_content_request(&user.user, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Approves a book/album request: adds it to the library (request settings defaults per
/// format) and starts a search. A failed add returns status `failed` with a `failure_reason`.
#[utoipa::path(post, path = "/api/v1/requests/media/{id}/approve", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Approved (or failed)", body = ContentRequestDto),
        (status = 403, description = "Requires MANAGE_REQUESTS", body = ApiErrorBody),
        (status = 409, description = "Not pending", body = ApiErrorBody),
    ))]
pub async fn approve_media_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<ContentRequestDto>> {
    Ok(ApiJson(
        svc(&state)?.approve_content_request(&user.user, id).await?,
    ))
}

/// Declines a book/album request with an optional reason.
#[utoipa::path(post, path = "/api/v1/requests/media/{id}/decline", tag = "requests", request_body = DeclineRequestDto,
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Declined", body = ContentRequestDto),
        (status = 403, description = "Requires MANAGE_REQUESTS", body = ApiErrorBody),
        (status = 409, description = "Not pending", body = ApiErrorBody),
    ))]
pub async fn decline_media_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    body: axum::body::Bytes,
) -> ApiResult<ApiJson<ContentRequestDto>> {
    let reason = decline_reason(&body)?;
    Ok(ApiJson(
        svc(&state)?
            .decline_content_request(&user.user, id, reason)
            .await?,
    ))
}

/// Retries a failed book/album request.
#[utoipa::path(post, path = "/api/v1/requests/media/{id}/retry", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Retried", body = ContentRequestDto),
        (status = 409, description = "Not failed", body = ApiErrorBody),
    ))]
pub async fn retry_media_request(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<ContentRequestDto>> {
    Ok(ApiJson(svc(&state)?.retry_content_request(&user.user, id).await?))
}

/// The optional decline body: `{}` / empty = no reason.
fn decline_reason(body: &[u8]) -> ApiResult<Option<String>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    Ok(serde_json::from_slice::<DeclineRequestDto>(body)
        .map_err(|e| ApiError::validation(format!("invalid body: {e}")))?
        .reason)
}

// ---------------------------------------------------------------- issues

/// Issues (own, or all with MANAGE_ISSUES), open first.
#[utoipa::path(get, path = "/api/v1/issues", tag = "requests", params(IssueListQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Page of issues", body = IssuePageDto)))]
pub async fn list_issues(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<IssueListQuery>,
) -> ApiResult<ApiJson<IssuePageDto>> {
    Ok(ApiJson(svc(&state)?.list_issues(&user.user, &q).await?))
}

/// Reports an issue (CREATE_ISSUES).
#[utoipa::path(post, path = "/api/v1/issues", tag = "requests", request_body = CreateIssueDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = IssueDto),
        (status = 403, description = "Requires CREATE_ISSUES", body = ApiErrorBody),
    ))]
pub async fn create_issue(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(dto): ApiJson<CreateIssueDto>,
) -> ApiResult<Response> {
    Ok(created(svc(&state)?.create_issue(&user.user, dto).await?))
}

/// Issue counts (scoped like the list).
#[utoipa::path(get, path = "/api/v1/issues/count", tag = "requests",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Counts", body = IssueCountsDto)))]
pub async fn issue_counts(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<IssueCountsDto>> {
    Ok(ApiJson(svc(&state)?.issue_counts(&user.user).await?))
}

/// One issue with its comment thread.
#[utoipa::path(get, path = "/api/v1/issues/{id}", tag = "requests",
    params(("id" = i64, Path, description = "Issue id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Issue", body = IssueDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_issue(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<IssueDto>> {
    Ok(ApiJson(svc(&state)?.get_issue(&user.user, id).await?))
}

/// Edits an issue (reporter or MANAGE_ISSUES).
#[utoipa::path(put, path = "/api/v1/issues/{id}", tag = "requests", request_body = UpdateIssueDto,
    params(("id" = i64, Path, description = "Issue id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = IssueDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn update_issue(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(dto): ApiJson<UpdateIssueDto>,
) -> ApiResult<ApiJson<IssueDto>> {
    Ok(ApiJson(svc(&state)?.update_issue(&user.user, id, dto).await?))
}

/// Deletes an issue (reporter while uncommented, or MANAGE_ISSUES).
#[utoipa::path(delete, path = "/api/v1/issues/{id}", tag = "requests",
    params(("id" = i64, Path, description = "Issue id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_issue(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    svc(&state)?.delete_issue(&user.user, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Adds a comment; returns the issue with its thread.
#[utoipa::path(post, path = "/api/v1/issues/{id}/comments", tag = "requests", request_body = CreateCommentDto,
    params(("id" = i64, Path, description = "Issue id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Comment added", body = IssueDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn add_comment(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(dto): ApiJson<CreateCommentDto>,
) -> ApiResult<Response> {
    Ok(created(svc(&state)?.add_comment(&user.user, id, dto).await?))
}

/// Marks an issue resolved.
#[utoipa::path(post, path = "/api/v1/issues/{id}/resolve", tag = "requests",
    params(("id" = i64, Path, description = "Issue id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Resolved", body = IssueDto)))]
pub async fn resolve_issue(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<IssueDto>> {
    Ok(ApiJson(
        svc(&state)?
            .set_issue_status(&user.user, id, IssueStatus::Resolved)
            .await?,
    ))
}

/// Reopens an issue.
#[utoipa::path(post, path = "/api/v1/issues/{id}/reopen", tag = "requests",
    params(("id" = i64, Path, description = "Issue id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Reopened", body = IssueDto)))]
pub async fn reopen_issue(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<IssueDto>> {
    Ok(ApiJson(
        svc(&state)?
            .set_issue_status(&user.user, id, IssueStatus::Open)
            .await?,
    ))
}

// ---------------------------------------------------------------- settings / profile

/// Request settings (admin).
#[utoipa::path(get, path = "/api/v1/settings/requests", tag = "requests",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Settings", body = RequestSettingsDto),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn get_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<RequestSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.request_settings().await?))
}

/// Replaces the request settings (admin).
#[utoipa::path(put, path = "/api/v1/settings/requests", tag = "requests", request_body = RequestSettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = RequestSettingsDto),
        (status = 400, description = "Invalid", body = ApiErrorBody),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn put_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<RequestSettingsDto>,
) -> ApiResult<ApiJson<RequestSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_request_settings(s).await?))
}

/// The current user's notification and watchlist preferences.
#[utoipa::path(get, path = "/api/v1/profile/notifications", tag = "requests",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Preferences", body = NotificationPrefsDto)))]
pub async fn get_prefs(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<NotificationPrefsDto>> {
    Ok(ApiJson(svc(&state)?.notification_prefs(user.user.id).await?))
}

/// Replaces the current user's notification and watchlist preferences.
#[utoipa::path(put, path = "/api/v1/profile/notifications", tag = "requests", request_body = NotificationPrefsDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = NotificationPrefsDto),
        (status = 400, description = "Invalid (e.g. watchlist sync without Plex)", body = ApiErrorBody),
    ))]
pub async fn put_prefs(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(p): ApiJson<NotificationPrefsDto>,
) -> ApiResult<ApiJson<NotificationPrefsDto>> {
    Ok(ApiJson(
        svc(&state)?.set_notification_prefs(user.user.id, p).await?,
    ))
}
