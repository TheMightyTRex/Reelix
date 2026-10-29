use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use reelix_api_types::{ApiErrorBody, ApiKeyDto, CreateApiKeyRequest, CreatedApiKey, LoginRequest, UserDto};

use crate::auth::{
    CurrentUser, LoginLimiter, RequestMeta, clear_cookie, generate_api_key, has_csrf_header, session_token,
    sha256_hex, start_session, user_dto, verify_password_blocking,
};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;

/// Logs in with a local username and password, setting the session cookie.
#[utoipa::path(post, path = "/api/v1/auth/login", tag = "auth", request_body = LoginRequest,
    responses(
        (status = 200, description = "Logged in; session cookie set", body = UserDto),
        (status = 401, description = "Invalid credentials", body = ApiErrorBody),
        (status = 429, description = "Too many failed attempts (see Retry-After)", body = ApiErrorBody),
    ))]
pub async fn login(
    State(state): State<AppState>,
    meta: RequestMeta,
    ApiJson(req): ApiJson<LoginRequest>,
) -> ApiResult<Response> {
    let key = LoginLimiter::key(meta.ip, &req.username);
    let limiter = &state.inner.limiter;
    limiter.check(&key)?;

    let found = state.db().users().find_credentials(req.username.trim()).await?;
    let (user, hash) = match found {
        Some((u, h)) => (Some(u), h),
        None => (None, None),
    };
    // Unknown users and users without a local password still pay for a (dummy) verify.
    let ok = verify_password_blocking(req.password, hash).await?;
    let Some(user) = user.filter(|_| ok) else {
        limiter.record_failure(&key);
        tracing::warn!(username = %req.username, ip = ?meta.ip, "failed login");
        return Err(ApiError::InvalidCredentials);
    };
    limiter.record_success(&key);

    // Replace any session this browser already had.
    if let Some(old) = session_token(&meta.headers) {
        state.db().sessions().delete(&sha256_hex(&old)).await?;
    }
    let cookie = start_session(&state, &meta, user.id, req.remember_me).await?;
    Ok(([(header::SET_COOKIE, cookie)], ApiJson(user_dto(&user))).into_response())
}

/// Ends the current session and clears the cookie.
#[utoipa::path(post, path = "/api/v1/auth/logout", tag = "auth",
    responses(
        (status = 204, description = "Logged out"),
        (status = 403, description = "Missing CSRF header", body = ApiErrorBody),
    ))]
pub async fn logout(State(state): State<AppState>, meta: RequestMeta) -> ApiResult<Response> {
    if let Some(token) = session_token(&meta.headers) {
        if !has_csrf_header(&meta.headers) {
            return Err(ApiError::Csrf);
        }
        state.db().sessions().delete(&sha256_hex(&token)).await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, clear_cookie(&state, &meta.headers))],
    )
        .into_response())
}

/// The currently authenticated user.
#[utoipa::path(get, path = "/api/v1/auth/me", tag = "auth",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Current user", body = UserDto),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
    ))]
pub async fn me(user: CurrentUser) -> ApiJson<UserDto> {
    ApiJson(user_dto(&user.user))
}

fn key_dto(k: reelix_db::ApiKey) -> ApiKeyDto {
    ApiKeyDto {
        id: k.id,
        name: k.name,
        prefix: k.prefix,
        created_at: k.created_at,
        last_used_at: k.last_used_at,
    }
}

/// Lists the current user's API keys.
#[utoipa::path(get, path = "/api/v1/auth/apikeys", tag = "auth",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "API keys", body = Vec<ApiKeyDto>),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
    ))]
pub async fn list_keys(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<ApiKeyDto>>> {
    let keys = state.db().api_keys().list_for_user(user.user.id).await?;
    Ok(ApiJson(keys.into_iter().map(key_dto).collect()))
}

/// Creates an API key for the current user. The full key is only returned here.
#[utoipa::path(post, path = "/api/v1/auth/apikeys", tag = "auth", request_body = CreateApiKeyRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = CreatedApiKey),
        (status = 400, description = "Invalid name", body = ApiErrorBody),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
    ))]
pub async fn create_key(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<CreateApiKeyRequest>,
) -> ApiResult<Response> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::validation("name must be 1-64 characters"));
    }
    let (key, prefix) = generate_api_key();
    let created = state
        .db()
        .api_keys()
        .create(user.user.id, name, &prefix, &sha256_hex(&key))
        .await?;
    let body = CreatedApiKey {
        api_key: key_dto(created),
        key,
    };
    Ok((StatusCode::CREATED, ApiJson(body)).into_response())
}

/// Revokes one of the current user's API keys.
#[utoipa::path(delete, path = "/api/v1/auth/apikeys/{id}", tag = "auth",
    params(("id" = i64, Path, description = "API key id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
        (status = 404, description = "No such key", body = ApiErrorBody),
    ))]
pub async fn delete_key(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    if state.db().api_keys().delete(user.user.id, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("API key not found"))
    }
}
