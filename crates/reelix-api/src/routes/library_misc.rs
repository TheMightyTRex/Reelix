//! Calendar (+ iCal feed), wanted lists, renaming and library settings.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use reelix_api_types::ApiErrorBody;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::library::{
    CalendarEntryDto, CalendarQuery, IcalQuery, MediaManagementSettings, NamingPreviewDto, NamingSettings,
    Paged, RenamePreviewDto, RenamePreviewQuery, RenameRequest, WantedItemDto, WantedQuery,
};
use reelix_core::Permissions;
use utoipa::OpenApi;

use super::{accepted, require_lib, svc};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiQuery, ApiResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/calendar", get(calendar))
        .route("/calendar.ics", get(calendar_ics))
        .route("/wanted/missing", get(wanted_missing))
        .route("/wanted/cutoff", get(wanted_cutoff))
        .route("/rename/preview", get(rename_preview))
        .route("/rename", post(rename))
        .route("/settings/naming", get(get_naming).put(put_naming))
        .route(
            "/naming/preview",
            get(naming_preview).post(naming_preview_unsaved),
        )
        .route(
            "/settings/mediamanagement",
            get(get_media_management).put(put_media_management),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        calendar,
        calendar_ics,
        wanted_missing,
        wanted_cutoff,
        rename_preview,
        rename,
        get_naming,
        put_naming,
        naming_preview,
        naming_preview_unsaved,
        get_media_management,
        put_media_management,
    ),
    components(schemas(
        reelix_api_types::library::CalendarEntryDto,
        reelix_api_types::library::CalendarKind,
        reelix_api_types::library::CalendarStatus,
        reelix_api_types::library::ReleaseKind,
        reelix_api_types::library::WantedItemDto,
        reelix_api_types::library::RenamePreviewDto,
        reelix_api_types::library::RenameRequest,
        reelix_api_types::library::NamingSettings,
        reelix_api_types::library::NamingPreviewDto,
        reelix_api_types::library::NamingErrorDto,
        reelix_api_types::library::ColonReplacement,
        reelix_api_types::library::MultiEpisodeStyle,
        reelix_api_types::library::MediaManagementSettings,
        reelix_api_types::library::ImportMode,
        reelix_api_types::library::RescanAfterRefresh,
    ))
)]
pub struct Api;

fn require_calendar(user: &CurrentUser) -> ApiResult<()> {
    if user.user.permissions.has(Permissions::MANAGE_LIBRARY)
        || user.user.permissions.has(Permissions::VIEW_CALENDAR)
    {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// Movie releases (cinema/digital/physical) and episode airings in a date range.
#[utoipa::path(get, path = "/api/v1/calendar", tag = "library", params(CalendarQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Entries by date", body = Vec<CalendarEntryDto>),
        (status = 403, description = "Requires MANAGE_LIBRARY or VIEW_CALENDAR", body = ApiErrorBody),
    ))]
pub async fn calendar(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<CalendarQuery>,
) -> ApiResult<ApiJson<Vec<CalendarEntryDto>>> {
    require_calendar(&user)?;
    Ok(ApiJson(svc(&state)?.calendar(&q).await?))
}

/// iCal feed (`text/calendar`); calendar apps authenticate with `?apikey=`.
#[utoipa::path(get, path = "/api/v1/calendar.ics", tag = "library", params(IcalQuery),
    security(("api_key" = []), ("session" = [])),
    responses((status = 200, description = "iCalendar text", content_type = "text/calendar", body = String)))]
pub async fn calendar_ics(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<IcalQuery>,
) -> ApiResult<Response> {
    require_calendar(&user)?;
    let body = svc(&state)?.ical(&q).await?;
    let mut res = body.into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/calendar; charset=utf-8"),
    );
    Ok(res)
}

/// Monitored, available items without a file (`kind=movie|episode`).
#[utoipa::path(get, path = "/api/v1/wanted/missing", tag = "library", params(WantedQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Missing", body = Paged<WantedItemDto>)))]
pub async fn wanted_missing(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<WantedQuery>,
) -> ApiResult<ApiJson<Paged<WantedItemDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.wanted_missing(&q).await?))
}

/// Monitored items whose file is below the profile cutoff (quality or format score).
#[utoipa::path(get, path = "/api/v1/wanted/cutoff", tag = "library", params(WantedQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Cutoff unmet", body = Paged<WantedItemDto>)))]
pub async fn wanted_cutoff(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<WantedQuery>,
) -> ApiResult<ApiJson<Paged<WantedItemDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.wanted_cutoff(&q).await?))
}

/// Files whose names differ from the naming templates (`movie_id` or `series_id`).
#[utoipa::path(get, path = "/api/v1/rename/preview", tag = "library", params(RenamePreviewQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Pending renames", body = Vec<RenamePreviewDto>)))]
pub async fn rename_preview(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<RenamePreviewQuery>,
) -> ApiResult<ApiJson<Vec<RenamePreviewDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.rename_preview(&q).await?))
}

/// Queues a `rename_files` job.
#[utoipa::path(post, path = "/api/v1/rename", tag = "library", request_body = RenameRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob)))]
pub async fn rename(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<RenameRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_rename(req).await?))
}

/// Naming templates.
#[utoipa::path(get, path = "/api/v1/settings/naming", tag = "library",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Naming settings", body = NamingSettings)))]
pub async fn get_naming(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<NamingSettings>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.naming().await?))
}

/// Saves naming templates (admin; validated).
#[utoipa::path(put, path = "/api/v1/settings/naming", tag = "library", request_body = NamingSettings,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = NamingSettings),
        (status = 400, description = "Invalid template", body = ApiErrorBody),
    ))]
pub async fn put_naming(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<NamingSettings>,
) -> ApiResult<ApiJson<NamingSettings>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_naming(s).await?))
}

/// Example names for the saved templates.
#[utoipa::path(get, path = "/api/v1/naming/preview", tag = "library",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Preview", body = NamingPreviewDto)))]
pub async fn naming_preview(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<NamingPreviewDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.naming_preview(None).await?))
}

/// Example names for unsaved templates (live preview while editing).
#[utoipa::path(post, path = "/api/v1/naming/preview", tag = "library", request_body = NamingSettings,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Preview + template errors", body = NamingPreviewDto)))]
pub async fn naming_preview_unsaved(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<NamingSettings>,
) -> ApiResult<ApiJson<NamingPreviewDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.naming_preview(Some(s)).await?))
}

/// Media management settings.
#[utoipa::path(get, path = "/api/v1/settings/mediamanagement", tag = "library",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Settings", body = MediaManagementSettings)))]
pub async fn get_media_management(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<MediaManagementSettings>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.media_management().await?))
}

/// Saves media management settings (admin).
#[utoipa::path(put, path = "/api/v1/settings/mediamanagement", tag = "library", request_body = MediaManagementSettings,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = MediaManagementSettings),
        (status = 400, description = "Invalid value", body = ApiErrorBody),
    ))]
pub async fn put_media_management(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<MediaManagementSettings>,
) -> ApiResult<ApiJson<MediaManagementSettings>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_media_management(s).await?))
}
