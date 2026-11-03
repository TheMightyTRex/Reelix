use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use reelix_api_types::ApiErrorBody;
use reelix_core::{Audience, DomainEvent, User};
use tokio::sync::broadcast::error::RecvError;

use crate::auth::{AuthMethod, Credential, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

/// How often an open socket re-checks that its session or API key is still valid.
const REVALIDATE_EVERY: Duration = Duration::from_secs(60);

/// Live event stream. Each text frame is a `DomainEvent` as `{ "type", "data" }` JSON,
/// filtered to what the authenticated user may see. Cookie-authenticated upgrades must come
/// from the same origin; the socket closes once the credential is revoked or the user's
/// permissions change.
#[utoipa::path(get, path = "/api/v1/ws", tag = "events",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 101, description = "Switching to the WebSocket protocol"),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
        (status = 403, description = "Cross-origin upgrade", body = ApiErrorBody),
    ))]
pub async fn ws(State(state): State<AppState>, req: Request) -> Response {
    let (mut parts, _) = req.into_parts();
    let user = match CurrentUser::from_request_parts(&mut parts, &state).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    // Browsers attach cookies to cross-site WebSocket handshakes, so check the Origin
    // (before the upgrade is negotiated) for cookie logins.
    if user.method == AuthMethod::Session && !same_origin(&parts.headers) {
        tracing::warn!(user = %user.user.username, "rejected cross-origin websocket");
        return ApiError::Forbidden.into_response();
    }
    let Some(cred) = Credential::from_parts(&parts) else {
        return ApiError::Unauthorized.into_response();
    };
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let rx = state.events().subscribe();
    upgrade.on_upgrade(move |socket| serve(socket, state, cred, user.user, rx))
}

/// Whether the `Origin` (if any; non-browser clients omit it) names the host the request was
/// sent to, as seen by us or by a reverse proxy (`X-Forwarded-Host`). Pages can't set either
/// header on a WebSocket handshake, so a foreign site can't satisfy this.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin) = origin.to_str().ok().and_then(|o| url::Url::parse(o).ok()) else {
        return false;
    };
    let authority = |host: &str| {
        url::Url::parse(&format!("{}://{}", origin.scheme(), host.trim()))
            .ok()
            .and_then(|u| Some((u.host_str()?.to_ascii_lowercase(), u.port_or_known_default())))
    };
    let Some(want) = authority(origin.authority()) else {
        return false;
    };
    [header::HOST.as_str(), "x-forwarded-host"]
        .iter()
        .filter_map(|h| headers.get(*h)?.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|host| authority(host) == Some(want.clone()))
}

/// Whether `user` may receive `event`.
pub fn visible_to(event: &DomainEvent, user: &User) -> bool {
    match event.audience() {
        Audience::All => true,
        Audience::Admin => user.permissions.is_admin(),
        Audience::User(id) => user.id == id || user.permissions.is_admin(),
    }
}

/// Whether `cred` still belongs to `user` with unchanged permissions (errors keep the socket).
async fn still_valid(state: &AppState, cred: &Credential, user: &User) -> bool {
    match cred.user(state).await {
        Ok(Some(u)) => u.id == user.id && u.permissions == user.permissions,
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(error = %e.to_string(), "websocket session check failed");
            true
        }
    }
}

async fn serve(
    mut socket: WebSocket,
    state: AppState,
    cred: Credential,
    user: User,
    mut rx: tokio::sync::broadcast::Receiver<DomainEvent>,
) {
    tracing::debug!(user = %user.username, "websocket connected");
    let mut revalidate = tokio::time::interval(REVALIDATE_EVERY);
    revalidate.reset();
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    // Edits and deletion of this user (password, permissions) take effect at once.
                    if let DomainEvent::EntityChanged { entity, id, .. } = &ev
                        && entity == "user"
                        && *id == user.id
                        && !still_valid(&state, &cred, &user).await
                    {
                        break;
                    }
                    if !visible_to(&ev, &user) {
                        continue;
                    }
                    let text = match serde_json::to_string(&ev) {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::error!(error = %e, "failed to serialise event");
                            continue;
                        }
                    };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, user = %user.username, "websocket client lagged");
                }
                Err(RecvError::Closed) => break,
            },
            _ = revalidate.tick() => {
                if !still_valid(&state, &cred, &user).await {
                    break;
                }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Ping(payload))) => {
                    if socket.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
    tracing::debug!(user = %user.username, "websocket disconnected");
}
