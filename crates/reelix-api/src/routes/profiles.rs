//! Quality profiles, quality definitions, custom formats (incl. TRaSH import/export),
//! delay profiles, release profiles and the release-name tester.
//!
//! Reads need `MANAGE_LIBRARY`; writes need `ADMIN`.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use reelix_api_types::ApiErrorBody;
use reelix_api_types::profiles::{
    ImportedFormatDto, MatchedFormatDto, ParseRequest, ParseResponse, ProfileDecisionDto, RejectionDto,
};
use reelix_core::{BookFormat, Permissions};
use reelix_decision::{
    CustomFormat, DecisionContext, DelayProfile, QualityDefinition, QualityProfile, RejectionKind,
    ReleaseCandidate, ReleaseProfile, SearchKind, trash,
};
use reelix_parser::QualityFamily;
use serde::Deserialize;
use serde_json::Value;
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/qualityprofiles", get(list_profiles).post(create_profile))
        .route(
            "/qualityprofiles/{id}",
            get(get_profile).put(update_profile).delete(delete_profile),
        )
        .route(
            "/qualitydefinitions",
            get(list_definitions).put(update_definitions),
        )
        .route("/customformats", get(list_formats).post(create_format))
        .route("/customformats/import", post(import_formats))
        .route(
            "/customformats/{id}",
            get(get_format).put(update_format).delete(delete_format),
        )
        .route("/customformats/{id}/export", get(export_format))
        .route("/delayprofiles", get(list_delay).post(create_delay))
        .route(
            "/delayprofiles/{id}",
            get(get_delay).put(update_delay).delete(delete_delay),
        )
        .route("/releaseprofiles", get(list_release).post(create_release))
        .route(
            "/releaseprofiles/{id}",
            get(get_release).put(update_release).delete(delete_release),
        )
        .route("/parse", post(parse))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_profiles, get_profile, create_profile, update_profile, delete_profile,
        list_definitions, update_definitions,
        list_formats, get_format, create_format, update_format, delete_format, import_formats, export_format,
        list_delay, get_delay, create_delay, update_delay, delete_delay,
        list_release, get_release, create_release, update_release, delete_release,
        parse,
    ),
    components(schemas(
        reelix_api_types::profiles::ParseRequest,
        reelix_api_types::profiles::ParseResponse,
        reelix_api_types::profiles::MatchedFormatDto,
        reelix_api_types::profiles::RejectionDto,
        reelix_api_types::profiles::ProfileDecisionDto,
        reelix_api_types::profiles::ImportedFormatDto,
    )),
    tags((name = "profiles", description = "Quality profiles, definitions, custom formats, delay/release profiles and the release parser")),
)]
pub struct Api;

fn created<T: serde::Serialize>(v: T) -> Response {
    (StatusCode::CREATED, ApiJson(v)).into_response()
}

fn deleted(found: bool, what: &str) -> ApiResult<StatusCode> {
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("{what} not found")))
    }
}

fn found<T>(v: Option<T>, what: &str) -> ApiResult<ApiJson<T>> {
    v.map(ApiJson)
        .ok_or_else(|| ApiError::not_found(format!("{what} not found")))
}

// ---------------- quality profiles ----------------

async fn validate_profile(state: &AppState, p: &QualityProfile) -> ApiResult<()> {
    p.validate().map_err(|e| ApiError::validation(e.to_string()))?;
    let formats = state.db().custom_formats().list().await?;
    if let Some(bad) = p
        .format_items
        .iter()
        .find(|i| !formats.iter().any(|f| f.id == i.format_id))
    {
        return Err(ApiError::validation(format!(
            "unknown custom format id {}",
            bad.format_id
        )));
    }
    Ok(())
}

/// `GET /qualityprofiles` query.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProfileListQuery {
    /// Quality family: `video` (default), `ebook`, `audiobook` or `music`.
    pub kind: Option<String>,
}

impl ProfileListQuery {
    /// The requested family; `None` when `kind` is absent or empty, 400 when unknown.
    fn family(&self) -> ApiResult<Option<QualityFamily>> {
        match self.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            None => Ok(None),
            Some(k) => QualityFamily::parse(k).map(Some).ok_or_else(|| {
                ApiError::validation(format!(
                    "unknown kind {k:?}: expected video, ebook, audiobook or music"
                ))
            }),
        }
    }
}

/// Quality profiles of one family (video by default).
#[utoipa::path(get, path = "/api/v1/qualityprofiles", tag = "profiles",
    params(ProfileListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Quality profiles (see reelix_decision::QualityProfile)", body = Vec<Object>),
        (status = 400, description = "Unknown kind", body = ApiErrorBody),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
    ))]
pub async fn list_profiles(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<ProfileListQuery>,
) -> ApiResult<ApiJson<Vec<QualityProfile>>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    let repo = state.db().quality_profiles();
    let profiles = match q.family()? {
        None | Some(QualityFamily::Video) => repo.list().await?,
        Some(f) => repo.list_family(f).await?,
    };
    Ok(ApiJson(profiles))
}

/// One quality profile.
#[utoipa::path(get, path = "/api/v1/qualityprofiles/{id}", tag = "profiles",
    params(("id" = i64, Path, description = "Profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Quality profile", body = Object),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<QualityProfile>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    found(
        state.db().quality_profiles().get_any(id).await?,
        "quality profile",
    )
}

/// Creates a quality profile (admin). Its family is `?kind=` when given, else the body's
/// `family` (default video); every listed quality must belong to that family.
#[utoipa::path(post, path = "/api/v1/qualityprofiles", tag = "profiles", request_body = Object,
    params(ProfileListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = Object),
        (status = 400, description = "Invalid profile", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
        (status = 409, description = "Duplicate name", body = ApiErrorBody),
    ))]
pub async fn create_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<ProfileListQuery>,
    ApiJson(mut p): ApiJson<QualityProfile>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    if let Some(f) = q.family()? {
        p.family = f;
    }
    validate_profile(&state, &p).await?;
    Ok(created(state.db().quality_profiles().create(&p).await?))
}

/// Replaces a quality profile (admin). The family can't change: the body is validated against
/// the stored profile's family.
#[utoipa::path(put, path = "/api/v1/qualityprofiles/{id}", tag = "profiles", request_body = Object,
    params(("id" = i64, Path, description = "Profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = Object),
        (status = 400, description = "Invalid profile", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
        (status = 409, description = "Duplicate name", body = ApiErrorBody),
    ))]
pub async fn update_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(mut p): ApiJson<QualityProfile>,
) -> ApiResult<ApiJson<QualityProfile>> {
    user.require(Permissions::ADMIN)?;
    let Some(current) = state.db().quality_profiles().get_any(id).await? else {
        return Err(ApiError::not_found("quality profile not found"));
    };
    p.family = current.family;
    validate_profile(&state, &p).await?;
    found(
        state.db().quality_profiles().update(id, &p).await?,
        "quality profile",
    )
}

/// Deletes a quality profile (admin).
#[utoipa::path(delete, path = "/api/v1/qualityprofiles/{id}", tag = "profiles",
    params(("id" = i64, Path, description = "Profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    deleted(state.db().quality_profiles().delete(id).await?, "quality profile")
}

// ---------------- quality definitions ----------------

/// Quality definitions of one family (video by default). Sizes are MB per minute of runtime,
/// except for ebooks (MB per book).
#[utoipa::path(get, path = "/api/v1/qualitydefinitions", tag = "profiles",
    params(ProfileListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Definitions in quality order", body = Vec<Object>),
        (status = 400, description = "Unknown kind", body = ApiErrorBody),
    ))]
pub async fn list_definitions(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<ProfileListQuery>,
) -> ApiResult<ApiJson<Vec<QualityDefinition>>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    let repo = state.db().quality_definitions();
    Ok(ApiJson(match q.family()? {
        None | Some(QualityFamily::Video) => repo.list().await?,
        Some(f) => repo.list_family(f).await?,
    }))
}

/// Bulk-updates quality definitions (admin). With `?kind=` every definition must belong to
/// that family and the family's full list is returned; without it any family is accepted and
/// the video list is returned.
#[utoipa::path(put, path = "/api/v1/qualitydefinitions", tag = "profiles", request_body = Vec<Object>,
    params(ProfileListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated definitions", body = Vec<Object>),
        (status = 400, description = "Invalid sizes, unknown kind or a quality of another family", body = ApiErrorBody),
    ))]
pub async fn update_definitions(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<ProfileListQuery>,
    ApiJson(defs): ApiJson<Vec<QualityDefinition>>,
) -> ApiResult<ApiJson<Vec<QualityDefinition>>> {
    user.require(Permissions::ADMIN)?;
    let family = q.family()?;
    for d in &defs {
        d.validate().map_err(ApiError::validation)?;
        if d.title.trim().is_empty() {
            return Err(ApiError::validation(format!(
                "{}: title is required",
                d.quality.name()
            )));
        }
        if let Some(f) = family
            && d.quality.family() != f
        {
            return Err(ApiError::validation(format!(
                "{} is not a {} quality",
                d.quality.name(),
                f.as_str()
            )));
        }
    }
    let repo = state.db().quality_definitions();
    Ok(ApiJson(match family {
        None => repo.update_all(&defs).await?,
        Some(f) => repo.update_family(f, &defs).await?,
    }))
}

// ---------------- custom formats ----------------

fn validate_format(f: &CustomFormat) -> ApiResult<()> {
    f.validate().map_err(|e| ApiError::validation(e.to_string()))
}

/// Custom formats.
#[utoipa::path(get, path = "/api/v1/customformats", tag = "profiles",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Custom formats", body = Vec<Object>)))]
pub async fn list_formats(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<CustomFormat>>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    Ok(ApiJson(state.db().custom_formats().list().await?))
}

/// One custom format.
#[utoipa::path(get, path = "/api/v1/customformats/{id}", tag = "profiles",
    params(("id" = i64, Path, description = "Custom format id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Custom format", body = Object),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_format(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<CustomFormat>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    found(state.db().custom_formats().get(id).await?, "custom format")
}

/// Creates a custom format (admin).
#[utoipa::path(post, path = "/api/v1/customformats", tag = "profiles", request_body = Object,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = Object),
        (status = 400, description = "Invalid format (e.g. bad regex)", body = ApiErrorBody),
        (status = 409, description = "Duplicate name", body = ApiErrorBody),
    ))]
pub async fn create_format(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(f): ApiJson<CustomFormat>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    validate_format(&f)?;
    Ok(created(state.db().custom_formats().create(&f, None).await?))
}

/// Replaces a custom format (admin).
#[utoipa::path(put, path = "/api/v1/customformats/{id}", tag = "profiles", request_body = Object,
    params(("id" = i64, Path, description = "Custom format id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = Object),
        (status = 400, description = "Invalid format", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn update_format(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(f): ApiJson<CustomFormat>,
) -> ApiResult<ApiJson<CustomFormat>> {
    user.require(Permissions::ADMIN)?;
    validate_format(&f)?;
    found(state.db().custom_formats().update(id, &f).await?, "custom format")
}

/// Deletes a custom format and removes its scores from every profile (admin).
#[utoipa::path(delete, path = "/api/v1/customformats/{id}", tag = "profiles",
    params(("id" = i64, Path, description = "Custom format id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_format(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    deleted(state.db().custom_formats().delete(id).await?, "custom format")
}

/// Imports TRaSH-guides / Radarr-export JSON (one format or an array) (admin).
/// All formats are validated before any is created.
#[utoipa::path(post, path = "/api/v1/customformats/import", tag = "profiles", request_body = Object,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created formats", body = Vec<ImportedFormatDto>),
        (status = 400, description = "Invalid or unsupported JSON", body = ApiErrorBody),
        (status = 409, description = "A format with that name exists", body = ApiErrorBody),
    ))]
pub async fn import_formats(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(body): ApiJson<Value>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let imported = trash::import_value(&body).map_err(|e| ApiError::validation(e.to_string()))?;
    if imported.is_empty() {
        return Err(ApiError::validation("no custom formats in the request"));
    }
    let existing = state.db().custom_formats().list().await?;
    if let Some(dup) = imported.iter().find(|i| {
        existing
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(i.format.name.trim()))
    }) {
        return Err(ApiError::Conflict(format!(
            "a custom format named {:?} already exists",
            dup.format.name
        )));
    }
    let mut out = Vec::with_capacity(imported.len());
    for i in imported {
        let format = state
            .db()
            .custom_formats()
            .create(&i.format, i.trash_id.as_deref())
            .await?;
        out.push(ImportedFormatDto {
            format,
            trash_id: i.trash_id,
            default_score: i.default_score,
        });
    }
    tracing::info!(count = out.len(), by = %user.user.username, "custom formats imported");
    Ok(created(out))
}

/// Exports a custom format as TRaSH-compatible JSON.
#[utoipa::path(get, path = "/api/v1/customformats/{id}/export", tag = "profiles",
    params(("id" = i64, Path, description = "Custom format id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "TRaSH JSON", body = Object),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn export_format(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<Value>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    let db = state.db();
    let f = db
        .custom_formats()
        .get(id)
        .await?
        .ok_or_else(|| ApiError::not_found("custom format not found"))?;
    let mut json = trash::export_json(&f, None);
    if let (Some(tid), Some(obj)) = (db.custom_formats().trash_id(id).await?, json.as_object_mut()) {
        obj.insert("trash_id".into(), Value::String(tid));
    }
    Ok(ApiJson(json))
}

// ---------------- delay profiles ----------------

fn validate_delay(p: &DelayProfile) -> ApiResult<()> {
    if !p.enable_usenet && !p.enable_torrent {
        return Err(ApiError::validation("enable at least one protocol"));
    }
    if p.usenet_delay > 60 * 24 * 30 || p.torrent_delay > 60 * 24 * 30 {
        return Err(ApiError::validation("delays are limited to 30 days"));
    }
    Ok(())
}

/// Delay profiles, in evaluation order.
#[utoipa::path(get, path = "/api/v1/delayprofiles", tag = "profiles",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Delay profiles", body = Vec<Object>)))]
pub async fn list_delay(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<DelayProfile>>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    Ok(ApiJson(state.db().delay_profiles().list().await?))
}

/// One delay profile.
#[utoipa::path(get, path = "/api/v1/delayprofiles/{id}", tag = "profiles",
    params(("id" = i64, Path, description = "Delay profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Delay profile", body = Object),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_delay(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<DelayProfile>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    found(state.db().delay_profiles().get(id).await?, "delay profile")
}

/// Creates a delay profile (admin).
#[utoipa::path(post, path = "/api/v1/delayprofiles", tag = "profiles", request_body = Object,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = Object),
        (status = 400, description = "Invalid profile", body = ApiErrorBody),
    ))]
pub async fn create_delay(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(p): ApiJson<DelayProfile>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    validate_delay(&p)?;
    Ok(created(state.db().delay_profiles().create(&p).await?))
}

/// Replaces a delay profile (admin).
#[utoipa::path(put, path = "/api/v1/delayprofiles/{id}", tag = "profiles", request_body = Object,
    params(("id" = i64, Path, description = "Delay profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = Object),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn update_delay(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(p): ApiJson<DelayProfile>,
) -> ApiResult<ApiJson<DelayProfile>> {
    user.require(Permissions::ADMIN)?;
    validate_delay(&p)?;
    found(state.db().delay_profiles().update(id, &p).await?, "delay profile")
}

/// Deletes a delay profile (admin). The untagged default profile cannot be deleted.
#[utoipa::path(delete, path = "/api/v1/delayprofiles/{id}", tag = "profiles",
    params(("id" = i64, Path, description = "Delay profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 400, description = "Default profile", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_delay(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    let repo = state.db().delay_profiles();
    if let Some(p) = repo.get(id).await?
        && p.tags.is_empty()
        && repo.list().await?.iter().filter(|d| d.tags.is_empty()).count() <= 1
    {
        return Err(ApiError::validation(
            "the default delay profile cannot be deleted",
        ));
    }
    deleted(repo.delete(id).await?, "delay profile")
}

// ---------------- release profiles ----------------

fn validate_release(p: &ReleaseProfile) -> ApiResult<()> {
    if p.required.is_empty() && p.ignored.is_empty() {
        return Err(ApiError::validation("add at least one required or ignored term"));
    }
    p.validate().map_err(|e| ApiError::validation(e.to_string()))
}

/// Release profiles.
#[utoipa::path(get, path = "/api/v1/releaseprofiles", tag = "profiles",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Release profiles", body = Vec<Object>)))]
pub async fn list_release(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<ReleaseProfile>>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    Ok(ApiJson(state.db().release_profiles().list().await?))
}

/// One release profile.
#[utoipa::path(get, path = "/api/v1/releaseprofiles/{id}", tag = "profiles",
    params(("id" = i64, Path, description = "Release profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Release profile", body = Object),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_release(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<ReleaseProfile>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    found(state.db().release_profiles().get(id).await?, "release profile")
}

/// Creates a release profile (admin). Terms are text or `/regex/i`.
#[utoipa::path(post, path = "/api/v1/releaseprofiles", tag = "profiles", request_body = Object,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = Object),
        (status = 400, description = "Invalid terms", body = ApiErrorBody),
    ))]
pub async fn create_release(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(p): ApiJson<ReleaseProfile>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    validate_release(&p)?;
    Ok(created(state.db().release_profiles().create(&p).await?))
}

/// Replaces a release profile (admin).
#[utoipa::path(put, path = "/api/v1/releaseprofiles/{id}", tag = "profiles", request_body = Object,
    params(("id" = i64, Path, description = "Release profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = Object),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn update_release(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(p): ApiJson<ReleaseProfile>,
) -> ApiResult<ApiJson<ReleaseProfile>> {
    user.require(Permissions::ADMIN)?;
    validate_release(&p)?;
    found(
        state.db().release_profiles().update(id, &p).await?,
        "release profile",
    )
}

/// Deletes a release profile (admin).
#[utoipa::path(delete, path = "/api/v1/releaseprofiles/{id}", tag = "profiles",
    params(("id" = i64, Path, description = "Release profile id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn delete_release(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    deleted(state.db().release_profiles().delete(id).await?, "release profile")
}

// ---------------- parse ----------------

/// Parses a release name and shows matched custom formats, score and (with `profile_id`)
/// whether the profile would accept it. A book or music profile parses the title as a book
/// or album release. File-dependent checks (upgrade, size vs runtime, delay, queue) are not
/// applied.
#[utoipa::path(post, path = "/api/v1/parse", tag = "profiles", request_body = ParseRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Parse result", body = ParseResponse),
        (status = 400, description = "Empty title", body = ApiErrorBody),
        (status = 404, description = "Unknown profile", body = ApiErrorBody),
    ))]
pub async fn parse(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<ParseRequest>,
) -> ApiResult<ApiJson<ParseResponse>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    let title = req.title.trim();
    if title.is_empty() || title.len() > 1000 {
        return Err(ApiError::validation("title must be 1-1000 characters"));
    }
    let db = state.db();
    let formats = db.custom_formats().list().await?;
    let profile = match req.profile_id {
        Some(id) => Some(
            db.quality_profiles()
                .get_any(id)
                .await?
                .ok_or_else(|| ApiError::not_found("quality profile not found"))?,
        ),
        None => None,
    };
    let release_profiles = db.release_profiles().list().await?;

    // Book/music profiles parse the title with their family's parser.
    let protocol = reelix_core::Protocol::Torrent;
    let mut candidate = match profile.as_ref().map(|p| p.family) {
        Some(QualityFamily::Ebook) => ReleaseCandidate::for_book(title, protocol, BookFormat::Ebook),
        Some(QualityFamily::Audiobook) => ReleaseCandidate::for_book(title, protocol, BookFormat::Audiobook),
        Some(QualityFamily::Music) => ReleaseCandidate::for_album(title, protocol),
        Some(QualityFamily::Video) | None => ReleaseCandidate::new(title, protocol),
    };
    candidate.size = req.size.unwrap_or(0);
    // Without a profile, evaluate against an empty one just to compute matched formats.
    let scratch = QualityProfile::with_allowed("", &[], reelix_parser::Quality::Unknown);
    let mut ctx = DecisionContext::new(profile.as_ref().unwrap_or(&scratch), chrono::Utc::now());
    ctx.formats = &formats;
    ctx.release_profiles = &release_profiles;
    ctx.search_kind = SearchKind::Interactive;
    let decision = reelix_decision::evaluate(&candidate, &ctx);

    let decision_dto = profile.as_ref().map(|p| ProfileDecisionDto {
        profile_id: p.id,
        profile_name: p.name.clone(),
        accepted: decision.accepted,
        rejections: decision
            .rejections
            .iter()
            .map(|r| RejectionDto {
                reason: r.reason.clone(),
                kind: match r.kind {
                    RejectionKind::Permanent => "permanent".into(),
                    RejectionKind::Temporary => "temporary".into(),
                },
                spec: r.spec.clone(),
            })
            .collect(),
        quality_rank: decision.quality_rank,
    });
    Ok(ApiJson(ParseResponse {
        quality: candidate.parsed.quality.quality.name().to_string(),
        parsed: candidate.parsed,
        matched_formats: decision
            .matched_formats
            .into_iter()
            .map(|f| MatchedFormatDto {
                id: f.id,
                name: f.name,
                score: f.score,
            })
            .collect(),
        format_score: decision.format_score,
        decision: decision_dto,
    }))
}
