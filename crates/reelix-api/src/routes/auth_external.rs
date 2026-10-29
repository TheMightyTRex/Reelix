//! External login (Jellyfin, Emby, Plex PIN, OIDC) and the security settings.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use reelix_api_types::ApiErrorBody;
use reelix_api_types::UserDto;
use reelix_api_types::requests::{
    AuthMethodsDto, MediaServerLoginDto, PlexPinDto, PlexPinExchangeDto, PlexPinRequestDto, PlexPinStatusDto,
    SecuritySettingsDto,
};
use reelix_core::{Permissions, User};
use reelix_requests::MediaServerKind;
use serde::Deserialize;
use utoipa::OpenApi;

use super::requests::svc;
use crate::auth::{
    CurrentUser, LoginLimiter, RequestMeta, session_token, sha256_hex, start_session, user_dto,
};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/methods", get(methods))
        .route("/auth/jellyfin", post(jellyfin))
        .route("/auth/emby", post(emby))
        .route("/auth/plex/pin", post(plex_pin))
        .route("/auth/plex/pin/{id}", post(plex_exchange))
        .route("/auth/oidc/login", get(oidc_login))
        .route("/auth/oidc/callback", get(oidc_callback))
        .route("/settings/security", get(get_security).put(put_security))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        methods,
        jellyfin,
        emby,
        plex_pin,
        plex_exchange,
        oidc_login,
        oidc_callback,
        get_security,
        put_security
    ),
    components(schemas(
        reelix_api_types::requests::AuthMethodsDto,
        reelix_api_types::requests::MediaServerLoginDto,
        reelix_api_types::requests::PlexPinRequestDto,
        reelix_api_types::requests::PlexPinDto,
        reelix_api_types::requests::PlexPinExchangeDto,
        reelix_api_types::requests::PlexPinStatusDto,
        reelix_api_types::requests::SecuritySettingsDto,
        reelix_api_types::requests::OidcSettingsDto,
    ))
)]
pub struct Api;

/// Replaces any existing session of this browser and starts one for `user`.
async fn login_response(
    state: &AppState,
    meta: &RequestMeta,
    user: &User,
    remember: bool,
) -> ApiResult<Response> {
    if let Some(old) = session_token(&meta.headers) {
        state.db().sessions().delete(&sha256_hex(&old)).await?;
    }
    let cookie = start_session(state, meta, user.id, remember).await?;
    Ok(([(header::SET_COOKIE, cookie)], ApiJson(user_dto(user))).into_response())
}

/// Which login methods are enabled (public).
#[utoipa::path(get, path = "/api/v1/auth/methods", tag = "auth",
    responses((status = 200, description = "Enabled login methods", body = AuthMethodsDto)))]
pub async fn methods(State(state): State<AppState>) -> ApiResult<ApiJson<AuthMethodsDto>> {
    Ok(ApiJson(svc(&state)?.auth_methods().await?))
}

async fn media_login(
    state: AppState,
    meta: RequestMeta,
    kind: MediaServerKind,
    req: MediaServerLoginDto,
) -> ApiResult<Response> {
    let key = LoginLimiter::key(meta.ip, &format!("{kind:?}:{}", req.username));
    let limiter = &state.inner.limiter;
    limiter.check(&key)?;
    match svc(&state)?
        .media_server_login(kind, &req.username, &req.password)
        .await
    {
        Ok(user) => {
            limiter.record_success(&key);
            login_response(&state, &meta, &user, req.remember_me).await
        }
        Err(e @ reelix_requests::Error::Unauthorized(_)) => {
            limiter.record_failure(&key);
            tracing::warn!(username = %req.username, ip = ?meta.ip, ?kind, "failed media server login");
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}

/// Logs in with Jellyfin credentials (creates the user on first login when enabled).
#[utoipa::path(post, path = "/api/v1/auth/jellyfin", tag = "auth", request_body = MediaServerLoginDto,
    responses(
        (status = 200, description = "Logged in; session cookie set", body = UserDto),
        (status = 401, description = "Invalid credentials / not linked", body = ApiErrorBody),
        (status = 403, description = "Method disabled", body = ApiErrorBody),
        (status = 429, description = "Too many failed attempts", body = ApiErrorBody),
    ))]
pub async fn jellyfin(
    State(state): State<AppState>,
    meta: RequestMeta,
    ApiJson(req): ApiJson<MediaServerLoginDto>,
) -> ApiResult<Response> {
    media_login(state, meta, MediaServerKind::Jellyfin, req).await
}

/// Logs in with Emby credentials.
#[utoipa::path(post, path = "/api/v1/auth/emby", tag = "auth", request_body = MediaServerLoginDto,
    responses(
        (status = 200, description = "Logged in; session cookie set", body = UserDto),
        (status = 401, description = "Invalid credentials / not linked", body = ApiErrorBody),
        (status = 403, description = "Method disabled", body = ApiErrorBody),
    ))]
pub async fn emby(
    State(state): State<AppState>,
    meta: RequestMeta,
    ApiJson(req): ApiJson<MediaServerLoginDto>,
) -> ApiResult<Response> {
    media_login(state, meta, MediaServerKind::Emby, req).await
}

/// Starts a Plex PIN login; open `auth_url` in a popup and poll `POST /auth/plex/pin/{id}`.
#[utoipa::path(post, path = "/api/v1/auth/plex/pin", tag = "auth", request_body = PlexPinRequestDto,
    responses(
        (status = 200, description = "PIN created", body = PlexPinDto),
        (status = 403, description = "Plex login disabled", body = ApiErrorBody),
    ))]
pub async fn plex_pin(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<PlexPinRequestDto>,
) -> ApiResult<ApiJson<PlexPinDto>> {
    Ok(ApiJson(
        svc(&state)?.plex_create_pin(req.forward_url.as_deref()).await?,
    ))
}

/// Checks a Plex PIN; once approved the account must own or be a friend of a configured Plex
/// server, then the session cookie is set.
#[utoipa::path(post, path = "/api/v1/auth/plex/pin/{id}", tag = "auth", request_body = PlexPinExchangeDto,
    params(("id" = i64, Path, description = "PIN id")),
    responses(
        (status = 200, description = "Status (authenticated + user when approved)", body = PlexPinStatusDto),
        (status = 403, description = "No access to a configured Plex server", body = ApiErrorBody),
    ))]
pub async fn plex_exchange(
    State(state): State<AppState>,
    meta: RequestMeta,
    ApiPath(id): ApiPath<i64>,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    let remember = serde_json::from_slice::<PlexPinExchangeDto>(&body)
        .map(|b| b.remember_me)
        .unwrap_or(false);
    match svc(&state)?.plex_check_pin(id).await? {
        None => Ok(ApiJson(PlexPinStatusDto {
            authenticated: false,
            user: None,
        })
        .into_response()),
        Some(user) => {
            if let Some(old) = session_token(&meta.headers) {
                state.db().sessions().delete(&sha256_hex(&old)).await?;
            }
            let cookie = start_session(&state, &meta, user.id, remember).await?;
            let body = PlexPinStatusDto {
                authenticated: true,
                user: Some(user_dto(&user)),
            };
            Ok(([(header::SET_COOKIE, cookie)], ApiJson(body)).into_response())
        }
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct OidcLoginQuery {
    /// In-app path to return to after login (relative, default `/`).
    redirect: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct OidcCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// `{scheme}://{host}{url_base}/api/v1/auth/oidc/callback` from (forwarded) request headers.
fn callback_url(state: &AppState, h: &HeaderMap) -> ApiResult<String> {
    let host = header_str(h, "x-forwarded-host")
        .or_else(|| header_str(h, "host"))
        .ok_or_else(|| ApiError::validation("cannot determine the public URL; set the OIDC redirect URL"))?;
    if host.contains(['/', '\\', '@', ' ']) {
        return Err(ApiError::validation("invalid Host header"));
    }
    let scheme = match header_str(h, "x-forwarded-proto") {
        Some(p) if p.eq_ignore_ascii_case("https") => "https",
        _ => "http",
    };
    Ok(format!(
        "{scheme}://{host}{}/api/v1/auth/oidc/callback",
        state.config().url_base
    ))
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn redirect(to: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, to.to_owned())]).into_response()
}

/// Redirects to the identity provider (authorization code + PKCE).
#[utoipa::path(get, path = "/api/v1/auth/oidc/login", tag = "auth", params(OidcLoginQuery),
    responses(
        (status = 302, description = "Redirect to the identity provider"),
        (status = 403, description = "OIDC disabled", body = ApiErrorBody),
        (status = 502, description = "Discovery failed", body = ApiErrorBody),
    ))]
pub async fn oidc_login(
    State(state): State<AppState>,
    meta: RequestMeta,
    ApiQuery(q): ApiQuery<OidcLoginQuery>,
) -> ApiResult<Response> {
    let svc = svc(&state)?;
    let cb = match svc.oidc_redirect_override().await? {
        Some(u) => u,
        None => callback_url(&state, &meta.headers)?,
    };
    let url = svc.oidc_begin(&cb, q.redirect.as_deref()).await?;
    Ok(redirect(&url))
}

/// Identity provider callback: verifies the ID token, starts a session and redirects into
/// the app (errors redirect to `/login?error=…`).
#[utoipa::path(get, path = "/api/v1/auth/oidc/callback", tag = "auth", params(OidcCallbackQuery),
    responses((status = 302, description = "Redirect into the app (cookie set) or to the login page")))]
pub async fn oidc_callback(
    State(state): State<AppState>,
    meta: RequestMeta,
    ApiQuery(q): ApiQuery<OidcCallbackQuery>,
) -> ApiResult<Response> {
    let base = state.config().url_base.clone();
    let fail = |msg: String| {
        tracing::warn!(error = %msg, ip = ?meta.ip, "OIDC login failed");
        Ok(redirect(&format!("{base}/login?error={}", percent_encode(&msg))))
    };
    if let Some(err) = q.error {
        return fail(q.error_description.unwrap_or(err));
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return fail("missing code or state".into());
    };
    let svc = svc(&state)?;
    match svc.oidc_complete(&code, &st).await {
        Ok((user, to)) => {
            if let Some(old) = session_token(&meta.headers) {
                state.db().sessions().delete(&sha256_hex(&old)).await?;
            }
            let cookie = start_session(&state, &meta, user.id, false).await?;
            Ok((
                StatusCode::FOUND,
                [
                    (header::SET_COOKIE, cookie),
                    (header::LOCATION, format!("{base}{to}").parse().expect("ascii")),
                ],
            )
                .into_response())
        }
        Err(e) => fail(e.to_string()),
    }
}

/// Security settings: login methods and OIDC (client secret masked).
#[utoipa::path(get, path = "/api/v1/settings/security", tag = "auth",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Settings", body = SecuritySettingsDto),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn get_security(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<SecuritySettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.security_settings_masked().await?))
}

/// Replaces the security settings (sending the masked secret back keeps it).
#[utoipa::path(put, path = "/api/v1/settings/security", tag = "auth", request_body = SecuritySettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = SecuritySettingsDto),
        (status = 400, description = "Invalid", body = ApiErrorBody),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn put_security(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<SecuritySettingsDto>,
) -> ApiResult<ApiJson<SecuritySettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_security_settings(s).await?))
}
