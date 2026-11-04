//! `POST /api/v1/command`: named commands that queue a job (`RefreshRadioShow`,
//! `RescanRadioLibrary`). Each command checks the permission of the module it belongs to;
//! add new names to [`command`].

use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use reelix_api_types::{ApiErrorBody, CommandRequest, EnqueuedJob};
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::routes::library::{accepted, require_lib};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/command", post(command))
}

#[derive(OpenApi)]
#[openapi(
    paths(command),
    components(schemas(reelix_api_types::CommandRequest)),
    tags((name = "command", description = "Named commands that queue a job")),
)]
pub struct Api;

/// Runs a named command: `RefreshRadioShow` (`show_ids`, empty = every show) or
/// `RescanRadioLibrary` (reads the podcast library and re-matches).
#[utoipa::path(post, path = "/api/v1/command", tag = "command", request_body = CommandRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued (job_id null when already queued)", body = EnqueuedJob),
        (status = 400, description = "Unknown command", body = ApiErrorBody),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
    ))]
pub async fn command(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<CommandRequest>,
) -> ApiResult<Response> {
    match req.name.as_str() {
        "RefreshRadioShow" => {
            require_lib(&user)?;
            let svc = crate::routes::radio::svc(&state)?;
            for id in &req.show_ids {
                svc.get_show(*id).await?;
            }
            Ok(accepted(svc.queue_refresh(req.show_ids).await?))
        }
        "RescanRadioLibrary" => {
            require_lib(&user)?;
            Ok(accepted(
                crate::routes::radio::svc(&state)?.queue_library_sync().await?,
            ))
        }
        other => Err(ApiError::validation(format!("unknown command {other}"))),
    }
}
