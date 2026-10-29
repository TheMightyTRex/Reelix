use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use reelix_api_types::ApiErrorBody;
use reelix_core::{Audience, DomainEvent, User};
use tokio::sync::broadcast::error::RecvError;

use crate::auth::CurrentUser;
use crate::state::AppState;

/// Live event stream. Each text frame is a `DomainEvent` as `{ "type", "data" }` JSON,
/// filtered to what the authenticated user may see.
#[utoipa::path(get, path = "/api/v1/ws", tag = "events",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 101, description = "Switching to the WebSocket protocol"),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
    ))]
pub async fn ws(State(state): State<AppState>, user: CurrentUser, upgrade: WebSocketUpgrade) -> Response {
    let rx = state.events().subscribe();
    upgrade.on_upgrade(move |socket| serve(socket, user.user, rx))
}

/// Whether `user` may receive `event`.
pub fn visible_to(event: &DomainEvent, user: &User) -> bool {
    match event.audience() {
        Audience::All => true,
        Audience::Admin => user.permissions.is_admin(),
        Audience::User(id) => user.id == id || user.permissions.is_admin(),
    }
}

async fn serve(mut socket: WebSocket, user: User, mut rx: tokio::sync::broadcast::Receiver<DomainEvent>) {
    tracing::debug!(user = %user.username, "websocket connected");
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
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
    tracing::debug!(user = %user.username, "websocket disconnected");
}
