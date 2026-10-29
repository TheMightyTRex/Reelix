//! User management (MANAGE_USERS): local users, role presets, media-server import.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use reelix_api_types::ApiErrorBody;
use reelix_api_types::requests::{
    CreateUserDto, ExternalUserDto, ImportServerDto, ImportUsersDto, ImportUsersResultDto, RolesDto,
    UpdateUserDto, UserDetailDto,
};
use utoipa::OpenApi;

use super::requests::svc;
use crate::auth::{CurrentUser, hash_password_blocking};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users", get(list_users).post(create_user))
        .route("/users/roles", get(roles))
        .route("/users/import/servers", get(import_servers))
        .route(
            "/users/import/{media_server_id}",
            get(import_preview).post(import_users),
        )
        .route("/users/{id}", get(get_user).put(update_user).delete(delete_user))
}

#[derive(OpenApi)]
#[openapi(
    paths(list_users, create_user, roles, import_servers, import_preview, import_users, get_user, update_user, delete_user),
    components(schemas(
        reelix_api_types::requests::UserDetailDto,
        reelix_api_types::requests::CreateUserDto,
        reelix_api_types::requests::UpdateUserDto,
        reelix_api_types::requests::RolePresetDto,
        reelix_api_types::requests::PermissionInfoDto,
        reelix_api_types::requests::RolesDto,
        reelix_api_types::requests::ImportServerDto,
        reelix_api_types::requests::ExternalUserDto,
        reelix_api_types::requests::ImportUsersDto,
        reelix_api_types::requests::ImportUsersResultDto,
    )),
    tags((name = "users", description = "User management, roles and media-server user import")),
)]
pub struct Api;

fn check_password(p: &str) -> ApiResult<()> {
    if p.chars().count() < 8 || p.len() > 1024 {
        return Err(ApiError::validation("password must be at least 8 characters"));
    }
    Ok(())
}

/// Every user.
#[utoipa::path(get, path = "/api/v1/users", tag = "users",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Users", body = Vec<UserDetailDto>),
        (status = 403, description = "Requires MANAGE_USERS", body = ApiErrorBody),
    ))]
pub async fn list_users(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<UserDetailDto>>> {
    Ok(ApiJson(svc(&state)?.list_users(&user.user).await?))
}

/// Creates a local user.
#[utoipa::path(post, path = "/api/v1/users", tag = "users", request_body = CreateUserDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = UserDetailDto),
        (status = 400, description = "Invalid", body = ApiErrorBody),
        (status = 409, description = "Username taken", body = ApiErrorBody),
    ))]
pub async fn create_user(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(dto): ApiJson<CreateUserDto>,
) -> ApiResult<Response> {
    let svc = svc(&state)?;
    // Check permissions before paying for the hash.
    svc.list_users(&user.user).await.map(|_| ())?;
    check_password(&dto.password)?;
    let hash = hash_password_blocking(dto.password.clone()).await?;
    let created = svc.create_user(&user.user, dto, hash).await?;
    Ok((StatusCode::CREATED, ApiJson(created)).into_response())
}

/// Role presets and the permission catalogue.
#[utoipa::path(get, path = "/api/v1/users/roles", tag = "users",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Roles", body = RolesDto)))]
pub async fn roles(_user: CurrentUser) -> ApiJson<RolesDto> {
    ApiJson(reelix_requests::users::roles())
}

/// Media servers users can be imported from.
#[utoipa::path(get, path = "/api/v1/users/import/servers", tag = "users",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Servers", body = Vec<ImportServerDto>)))]
pub async fn import_servers(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<ImportServerDto>>> {
    Ok(ApiJson(svc(&state)?.import_servers(&user.user).await?))
}

/// Accounts on a media server, with the Reelix user they are linked to.
#[utoipa::path(get, path = "/api/v1/users/import/{media_server_id}", tag = "users",
    params(("media_server_id" = i64, Path, description = "Media server provider id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Accounts", body = Vec<ExternalUserDto>),
        (status = 404, description = "Unknown server", body = ApiErrorBody),
        (status = 502, description = "Server unreachable", body = ApiErrorBody),
    ))]
pub async fn import_preview(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<Vec<ExternalUserDto>>> {
    Ok(ApiJson(svc(&state)?.import_preview(&user.user, id).await?))
}

/// Imports (selected) media-server accounts as Reelix users.
#[utoipa::path(post, path = "/api/v1/users/import/{media_server_id}", tag = "users", request_body = ImportUsersDto,
    params(("media_server_id" = i64, Path, description = "Media server provider id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Imported", body = ImportUsersResultDto)))]
pub async fn import_users(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(dto): ApiJson<ImportUsersDto>,
) -> ApiResult<ApiJson<ImportUsersResultDto>> {
    Ok(ApiJson(svc(&state)?.import_users(&user.user, id, dto).await?))
}

/// One user (self, or any with MANAGE_USERS).
#[utoipa::path(get, path = "/api/v1/users/{id}", tag = "users",
    params(("id" = i64, Path, description = "User id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "User", body = UserDetailDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_user(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<UserDetailDto>> {
    Ok(ApiJson(svc(&state)?.get_user(&user.user, id).await?))
}

/// Updates a user (MANAGE_USERS; users may edit their own profile fields and password).
#[utoipa::path(put, path = "/api/v1/users/{id}", tag = "users", request_body = UpdateUserDto,
    params(("id" = i64, Path, description = "User id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = UserDetailDto),
        (status = 403, description = "Not permitted", body = ApiErrorBody),
        (status = 409, description = "Would remove the last admin", body = ApiErrorBody),
    ))]
pub async fn update_user(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(mut dto): ApiJson<UpdateUserDto>,
) -> ApiResult<ApiJson<UserDetailDto>> {
    let hash = match dto.password.take().filter(|p| !p.is_empty()) {
        Some(p) => {
            check_password(&p)?;
            Some(hash_password_blocking(p).await?)
        }
        None => None,
    };
    Ok(ApiJson(
        svc(&state)?.update_user(&user.user, id, dto, hash).await?,
    ))
}

/// Deletes a user and their requests/issues.
#[utoipa::path(delete, path = "/api/v1/users/{id}", tag = "users",
    params(("id" = i64, Path, description = "User id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 409, description = "Self or last admin", body = ApiErrorBody),
    ))]
pub async fn delete_user(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    svc(&state)?.delete_user(&user.user, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
