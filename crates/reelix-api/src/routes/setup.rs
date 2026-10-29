use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use reelix_api_types::{ApiErrorBody, SetupRequest, SetupStatus, UserDto};
use reelix_core::{AuthSource, DomainEvent, Permissions, VERSION};
use reelix_db::NewUser;

use crate::auth::{RequestMeta, hash_password_blocking, start_session, user_dto};
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::state::AppState;

/// Whether first-run setup is still required.
#[utoipa::path(get, path = "/api/v1/setup", tag = "setup",
    responses((status = 200, description = "Setup status", body = SetupStatus)))]
pub async fn status(State(state): State<AppState>) -> ApiResult<ApiJson<SetupStatus>> {
    let needs_setup = state.db().users().count().await? == 0;
    Ok(ApiJson(SetupStatus {
        needs_setup,
        version: VERSION.to_owned(),
    }))
}

/// Creates the first admin account and logs it in. Only allowed while no users exist.
#[utoipa::path(post, path = "/api/v1/setup", tag = "setup", request_body = SetupRequest,
    responses(
        (status = 201, description = "Admin created; session cookie set", body = UserDto),
        (status = 400, description = "Invalid input", body = ApiErrorBody),
        (status = 409, description = "Setup already completed", body = ApiErrorBody),
    ))]
pub async fn complete(
    State(state): State<AppState>,
    meta: RequestMeta,
    ApiJson(req): ApiJson<SetupRequest>,
) -> ApiResult<Response> {
    let username = req.username.trim().to_owned();
    validate_username(&username)?;
    validate_password(&req.password)?;
    let email = req.email.map(|e| e.trim().to_owned()).filter(|e| !e.is_empty());
    if let Some(e) = &email {
        validate_email(e)?;
    }

    let _guard = state.inner.setup_lock.lock().await;
    if state.db().users().count().await? > 0 {
        return Err(ApiError::Conflict("setup has already been completed".into()));
    }
    let password_hash = hash_password_blocking(req.password).await?;
    let user = state
        .db()
        .users()
        .create(NewUser {
            username,
            email,
            display_name: None,
            password_hash: Some(password_hash),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_ADMIN,
        })
        .await?;
    let cookie = start_session(&state, &meta, user.id, false).await?;
    tracing::info!(user = %user.username, "first-run setup completed");
    state.events().publish(DomainEvent::UserCreated {
        user_id: user.id,
        username: user.username.clone(),
    });
    state
        .events()
        .publish(DomainEvent::SetupCompleted { user_id: user.id });
    Ok((
        StatusCode::CREATED,
        [(header::SET_COOKIE, cookie)],
        ApiJson(user_dto(&user)),
    )
        .into_response())
}

pub(crate) fn validate_username(name: &str) -> ApiResult<()> {
    let ok_chars = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if name.is_empty() || name.len() > 64 || !ok_chars {
        return Err(ApiError::validation(
            "username must be 1-64 characters of A-Z, a-z, 0-9, '.', '_' or '-'",
        ));
    }
    Ok(())
}

pub(crate) fn validate_password(pw: &str) -> ApiResult<()> {
    if pw.chars().count() < 8 {
        return Err(ApiError::validation("password must be at least 8 characters"));
    }
    if pw.len() > 1024 {
        return Err(ApiError::validation("password must be at most 1024 bytes"));
    }
    Ok(())
}

fn validate_email(e: &str) -> ApiResult<()> {
    let valid = e.len() <= 254
        && e.split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        && !e.chars().any(char::is_whitespace);
    if valid {
        Ok(())
    } else {
        Err(ApiError::validation("email address is invalid"))
    }
}
