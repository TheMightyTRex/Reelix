//! Radarr's and Sonarr's SignalR hub (`/radarr/signalr/messages`, `/sonarr/signalr/messages`),
//! first step (RFX-76).
//!
//! Bazarr connects to this hub for live updates. Its client (`signalrcore`) first posts to
//! `…/signalr/messages/negotiate?access_token=<key>` and treats any answer but 200 as a fatal
//! error, so a 405 there filled Bazarr's log. It has no other way to fall back: it learns about
//! new films and episodes from its scheduled sync whenever the hub sends nothing.
//!
//! So this hub negotiates, completes the JSON hub protocol handshake and keeps the connection
//! alive with pings, but sends no events yet. Bazarr then reports the feed as connected and
//! carries on with its scheduled sync, quietly. The movie, series, episode and file events of
//! the full feed are still to come.
//!
//! Only API keys are accepted (the `X-Api-Key` header, `?apikey=` or SignalR's
//! `?access_token=`), never a session cookie, so other sites cannot open the socket in a
//! signed-in browser.

use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use reelix_api_types::API_KEY_HEADER;
use reelix_core::{Permissions, User};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::sha256_hex;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Ends every SignalR JSON hub protocol message.
const RS: char = '\u{1e}';
/// How often the hub pings an idle connection (SignalR's default keep-alive).
const PING_EVERY: Duration = Duration::from_secs(15);
/// How often an open connection checks that its API key is still valid.
const REVALIDATE_EVERY: Duration = Duration::from_secs(60);
/// The longest a client may take to send its handshake.
const HANDSHAKE_WITHIN: Duration = Duration::from_secs(15);

/// `/signalr/messages` and its negotiate route, nested under `/radarr` and `/sonarr`.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/messages", get(hub))
        .route("/messages/negotiate", post(negotiate))
}

#[derive(Debug, Default, Deserialize)]
struct KeyQuery {
    apikey: Option<String>,
    access_token: Option<String>,
}

/// The API key a hub request carries: the header, then `?apikey=`, then `?access_token=`.
fn api_key(headers: &HeaderMap, q: &KeyQuery) -> Option<String> {
    let given = |k: Option<&str>| k.map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned);
    given(headers.get(API_KEY_HEADER).and_then(|v| v.to_str().ok()))
        .or_else(|| given(q.apikey.as_deref()))
        .or_else(|| given(q.access_token.as_deref()))
}

/// The user of `key` when it is valid and may manage the library, as the *arr routes require.
async fn key_user(state: &AppState, key: Option<&str>) -> ApiResult<User> {
    let key = key.ok_or(ApiError::Unauthorized)?;
    let user = state
        .db()
        .api_keys()
        .find_user(&sha256_hex(key))
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if !user.permissions.has(Permissions::MANAGE_LIBRARY) {
        return Err(ApiError::Forbidden);
    }
    Ok(user)
}

/// `POST …/signalr/messages/negotiate`: offers the WebSocket transport, as Radarr and Sonarr do.
async fn negotiate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<KeyQuery>,
) -> ApiResult<axum::Json<Value>> {
    key_user(&state, api_key(&headers, &q).as_deref()).await?;
    Ok(axum::Json(json!({
        "negotiateVersion": 0,
        "connectionId": crate::auth::random_token(),
        "availableTransports": [{"transport": "WebSockets", "transferFormats": ["Text"]}],
    })))
}

/// `GET …/signalr/messages`: the hub's WebSocket.
async fn hub(State(state): State<AppState>, req: Request) -> Response {
    let (mut parts, _) = req.into_parts();
    let q = Query::<KeyQuery>::try_from_uri(&parts.uri)
        .map(|q| q.0)
        .unwrap_or_default();
    let Some(key) = api_key(&parts.headers, &q) else {
        return ApiError::Unauthorized.into_response();
    };
    let user = match key_user(&state, Some(&key)).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    upgrade.on_upgrade(move |socket| serve(socket, state, key, user))
}

/// One hub message: the JSON followed by the record separator.
fn record(v: &Value) -> Message {
    Message::Text(format!("{v}{RS}").into())
}

/// The handshake answer for `text`: `{}` for the JSON protocol, otherwise an error.
fn handshake_reply(text: &str) -> Result<Value, Value> {
    let first = text.split(RS).next().unwrap_or_default();
    let ask: Value = serde_json::from_str(first).unwrap_or(Value::Null);
    match ask.get("protocol").and_then(Value::as_str) {
        Some("json") => Ok(json!({})),
        Some(p) => Err(json!({"error": format!("protocol '{p}' is not supported; use json")})),
        None => Err(json!({"error": "handshake expected"})),
    }
}

/// Whether `text` holds a close message (type 7) from the client.
fn closes(text: &str) -> bool {
    text.split(RS)
        .filter(|r| !r.trim().is_empty())
        .filter_map(|r| serde_json::from_str::<Value>(r).ok())
        .any(|m| m.get("type").and_then(Value::as_i64) == Some(7))
}

async fn serve(mut socket: WebSocket, state: AppState, key: String, user: User) {
    tracing::debug!(user = %user.username, "SignalR hub connected (no live events yet)");
    let first = tokio::time::timeout(HANDSHAKE_WITHIN, socket.recv()).await;
    let text = match first {
        Ok(Some(Ok(Message::Text(t)))) => t.as_str().to_owned(),
        _ => return,
    };
    match handshake_reply(&text) {
        Ok(v) => {
            if socket.send(record(&v)).await.is_err() {
                return;
            }
        }
        Err(v) => {
            let _ = socket.send(record(&v)).await;
            return;
        }
    }
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.reset();
    let mut revalidate = tokio::time::interval(REVALIDATE_EVERY);
    revalidate.reset();
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) if closes(t.as_str()) => break,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                // Pings and anything else the client sends need no answer.
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if socket.send(record(&json!({"type": 6}))).await.is_err() {
                    break;
                }
            }
            _ = revalidate.tick() => {
                let still = matches!(key_user(&state, Some(&key)).await, Ok(u) if u.id == user.id);
                if !still {
                    let _ = socket
                        .send(record(&json!({"type": 7, "error": "the API key is no longer valid"})))
                        .await;
                    break;
                }
            }
        }
    }
    tracing::debug!(user = %user.username, "SignalR hub disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_accepts_json_only() {
        assert_eq!(
            handshake_reply("{\"protocol\":\"json\",\"version\":1}\u{1e}"),
            Ok(json!({}))
        );
        assert!(handshake_reply("{\"protocol\":\"messagepack\",\"version\":1}\u{1e}").is_err());
        assert!(handshake_reply("{\"type\":6}\u{1e}").is_err());
    }

    #[test]
    fn close_messages_are_found_among_others() {
        assert!(closes("{\"type\":6}\u{1e}{\"type\":7}\u{1e}"));
        assert!(!closes("{\"type\":6}\u{1e}"));
    }
}
