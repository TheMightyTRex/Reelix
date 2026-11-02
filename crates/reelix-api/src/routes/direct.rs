//! Admin-only direct-download queue and provider settings.
use crate::{ApiError, ApiResult, AppState, CurrentUser, error::ApiJson};
use axum::{
    Router,
    extract::{Path, State},
    routing::{get, post},
};
use reelix_api_types::direct::*;
use reelix_core::Permissions;
use reelix_direct::DirectService;
use utoipa::OpenApi;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/downloads", get(list).post(enqueue))
        .route("/downloads/settings", get(settings).put(save_settings))
        .route("/downloads/health", get(health))
        .route("/downloads/preview", post(preview))
        .route("/downloads/{id}/cancel", post(cancel))
        .route("/downloads/{id}/retry", post(retry))
        .route("/downloads/spotify", get(spotify_status))
        .route("/downloads/spotify/login", post(spotify_login))
        .route("/downloads/spotify/login/finish", post(spotify_login_finish))
        .route("/downloads/spotify/disconnect", post(spotify_disconnect))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list,
        enqueue,
        settings,
        save_settings,
        health,
        preview,
        cancel,
        retry,
        spotify_status,
        spotify_login,
        spotify_login_finish,
        spotify_disconnect
    ),
    components(schemas(
        DirectSettings,
        DirectSource,
        DownloadMode,
        DirectRequest,
        DirectDownload,
        DirectHealth,
        DirectPreview,
        SpotifyLoginStart,
        SpotifyLoginFinish,
        SpotifyStatus
    ))
)]
pub struct Api;

#[utoipa::path(get, path="/api/v1/downloads", tag="downloads", security(("session"=[]),("api_key"=[])), responses((status=200,body=Vec<DirectDownload>)))]
async fn list(State(s): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<Vec<DirectDownload>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .list()
            .await
            .map_err(ApiError::internal)?,
    ))
}

#[utoipa::path(post, path="/api/v1/downloads", tag="downloads", request_body=DirectRequest, security(("session"=[]),("api_key"=[])), responses((status=200,body=DirectDownload)))]
async fn enqueue(
    State(s): State<AppState>,
    user: CurrentUser,
    ApiJson(r): ApiJson<DirectRequest>,
) -> ApiResult<ApiJson<DirectDownload>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .enqueue(r)
            .await
            .map_err(|e| ApiError::validation(e.to_string()))?,
    ))
}

#[utoipa::path(get, path="/api/v1/downloads/settings", tag="downloads", security(("session"=[]),("api_key"=[])), responses((status=200,body=DirectSettings)))]
async fn settings(State(s): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<DirectSettings>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .settings()
            .await
            .map_err(ApiError::internal)?,
    ))
}

#[utoipa::path(put, path="/api/v1/downloads/settings", tag="downloads", request_body=DirectSettings, security(("session"=[]),("api_key"=[])), responses((status=200)))]
async fn save_settings(
    State(s): State<AppState>,
    user: CurrentUser,
    ApiJson(r): ApiJson<DirectSettings>,
) -> ApiResult<ApiJson<()>> {
    user.require(Permissions::ADMIN)?;
    s.ext::<DirectService>()?
        .save_settings(r)
        .await
        .map_err(|e| ApiError::validation(e.to_string()))?;
    Ok(ApiJson(()))
}

#[utoipa::path(get, path="/api/v1/downloads/health", tag="downloads", security(("session"=[]),("api_key"=[])), responses((status=200,body=Vec<DirectHealth>)))]
async fn health(State(s): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<Vec<DirectHealth>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .health()
            .await
            .map_err(ApiError::internal)?,
    ))
}

#[utoipa::path(post, path="/api/v1/downloads/preview", tag="downloads", request_body=DirectRequest, security(("session"=[]),("api_key"=[])), responses((status=200,body=DirectPreview)))]
async fn preview(
    State(s): State<AppState>,
    user: CurrentUser,
    ApiJson(r): ApiJson<DirectRequest>,
) -> ApiResult<ApiJson<DirectPreview>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .preview(r)
            .await
            .map_err(|e| ApiError::validation(e.to_string()))?,
    ))
}

#[utoipa::path(post, path="/api/v1/downloads/{id}/cancel", tag="downloads", params(("id"=i64,Path)), security(("session"=[]),("api_key"=[])), responses((status=200)))]
async fn cancel(State(s): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> ApiResult<ApiJson<()>> {
    user.require(Permissions::ADMIN)?;
    s.ext::<DirectService>()?
        .cancel(id)
        .await
        .map_err(|e| ApiError::validation(e.to_string()))?;
    Ok(ApiJson(()))
}

#[utoipa::path(post, path="/api/v1/downloads/{id}/retry", tag="downloads", params(("id"=i64,Path)), security(("session"=[]),("api_key"=[])), responses((status=200)))]
async fn retry(State(s): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> ApiResult<ApiJson<()>> {
    user.require(Permissions::ADMIN)?;
    s.ext::<DirectService>()?
        .retry(id)
        .await
        .map_err(|e| ApiError::validation(e.to_string()))?;
    Ok(ApiJson(()))
}

#[utoipa::path(get, path="/api/v1/downloads/spotify", tag="downloads", security(("session"=[]),("api_key"=[])), responses((status=200,body=SpotifyStatus)))]
async fn spotify_status(State(s): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<SpotifyStatus>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .spotify_status()
            .await
            .map_err(ApiError::internal)?,
    ))
}

#[utoipa::path(post, path="/api/v1/downloads/spotify/login", tag="downloads", security(("session"=[]),("api_key"=[])), responses((status=200,body=SpotifyLoginStart)))]
async fn spotify_login(State(s): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<SpotifyLoginStart>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .spotify_login_start()
            .await
            .map_err(ApiError::internal)?,
    ))
}

#[utoipa::path(post, path="/api/v1/downloads/spotify/login/finish", tag="downloads", request_body=SpotifyLoginFinish, security(("session"=[]),("api_key"=[])), responses((status=200,body=SpotifyStatus)))]
async fn spotify_login_finish(
    State(s): State<AppState>,
    user: CurrentUser,
    ApiJson(r): ApiJson<SpotifyLoginFinish>,
) -> ApiResult<ApiJson<SpotifyStatus>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .spotify_login_finish(&r.redirect_url)
            .await
            .map_err(|e| ApiError::validation(e.to_string()))?,
    ))
}

#[utoipa::path(post, path="/api/v1/downloads/spotify/disconnect", tag="downloads", security(("session"=[]),("api_key"=[])), responses((status=200,body=SpotifyStatus)))]
async fn spotify_disconnect(State(s): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<SpotifyStatus>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        s.ext::<DirectService>()?
            .spotify_disconnect()
            .await
            .map_err(ApiError::internal)?,
    ))
}
