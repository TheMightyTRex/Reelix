//! Radarr's and Sonarr's SignalR hub (`/radarr/signalr/messages`, `/sonarr/signalr/messages`)
//! (RFX-76, fork issue #6).
//!
//! Bazarr connects to this hub for live updates. Its client (`signalrcore`) first posts to
//! `…/signalr/messages/negotiate?access_token=<key>` and treats any answer but 200 as a fatal
//! error, so a 405 there filled Bazarr's log.
//!
//! The hub negotiates, completes the JSON hub protocol handshake, keeps the connection alive
//! with pings, and sends the library's changes as Radarr and Sonarr do: a `receiveMessage`
//! invocation whose argument is `{name, body: {action, resource}}`. Radarr's hub sends `movie`
//! messages; Sonarr's sends `series` and `episode` messages. A new or replaced file is sent as
//! its film or episode, `updated`, as Bazarr reads the file from there. A deleted item carries
//! only its `id`, as its record is gone. Changes are gathered for [`FLUSH_EVERY`] and each item
//! is sent once, so a library scan does not send the same film many times.
//!
//! Only API keys are accepted (the `X-Api-Key` header, `?apikey=` or SignalR's
//! `?access_token=`), never a session cookie, so other sites cannot open the socket in a
//! signed-in browser.

use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use std::collections::BTreeMap;

use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use reelix_api_types::API_KEY_HEADER;
use reelix_core::{DomainEvent, Permissions, User};
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
/// How long library changes are gathered before they are sent, each item once.
const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// Most items sent in one go. In a bigger burst (a whole library refresh) only additions and
/// deletions are sent, and updates are left to the client's scheduled sync, as they would make
/// the client read every item at once.
const MAX_BATCH: usize = 500;

/// Which app's hub: Radarr's sends film changes, Sonarr's series and episode changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hub {
    Radarr,
    Sonarr,
}

/// `/signalr/messages` and its negotiate route, nested under `/radarr` and `/sonarr`.
pub(super) fn routes(which: Hub) -> Router<AppState> {
    Router::new()
        .route(
            "/messages",
            get(move |state: State<AppState>, req: Request| hub(state, req, which)),
        )
        .route("/messages/negotiate", post(negotiate))
}

/// What a library change asks this hub to send, before the record is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Item {
    Movie(i64),
    Series(i64),
    Episode(i64),
    /// A film's new file: sent as its film.
    MovieFile(i64),
    /// An episode file: sent as its episodes.
    EpisodeFile(i64),
}

/// The item and action (`created`, `updated` or `deleted`) `event` asks `which` to send, if any.
fn wanted(which: Hub, event: &DomainEvent) -> Option<(Item, &'static str)> {
    let DomainEvent::EntityChanged {
        entity, action, id, ..
    } = event
    else {
        return None;
    };
    let action = match action.as_str() {
        "created" => "created",
        "updated" => "updated",
        "deleted" => "deleted",
        _ => return None,
    };
    let id = *id;
    let item = match (which, entity.as_str()) {
        (Hub::Radarr, "movie") => Item::Movie(id),
        (Hub::Sonarr, "series") => Item::Series(id),
        (Hub::Sonarr, "episode") => Item::Episode(id),
        // A deleted file's film or episode is no longer known from the file.
        (Hub::Radarr, "movie_file") if action != "deleted" => Item::MovieFile(id),
        (Hub::Sonarr, "episode_file") if action != "deleted" => Item::EpisodeFile(id),
        _ => return None,
    };
    let action = if matches!(item, Item::MovieFile(_) | Item::EpisodeFile(_)) {
        "updated"
    } else {
        action
    };
    Some((item, action))
}

/// Adds a change to the gathered ones: a later action replaces an earlier one, except that a
/// creation stays a creation until it is deleted.
fn gather(pending: &mut BTreeMap<Item, &'static str>, item: Item, action: &'static str) {
    let next = match (pending.get(&item).copied(), action) {
        (Some("created"), "updated") => "created",
        _ => action,
    };
    pending.insert(item, next);
}

/// A `receiveMessage` invocation, as Radarr and Sonarr send it.
fn message(name: &str, action: &str, resource: Value) -> Value {
    json!({
        "type": 1,
        "target": "receiveMessage",
        "arguments": [{"name": name, "body": {"action": action, "resource": resource}}],
    })
}

/// The messages for one gathered change; an item that has gone since is left out, as its
/// deletion is sent on its own.
async fn messages_for(state: &AppState, item: Item, action: &str) -> Vec<Value> {
    let Ok(lib) = super::library(state) else {
        return Vec::new();
    };
    let gone = |name: &str, id: i64| vec![message(name, "deleted", json!({"id": id}))];
    match item {
        Item::Movie(id) if action == "deleted" => gone("movie", id),
        Item::Series(id) if action == "deleted" => gone("series", id),
        Item::Episode(id) if action == "deleted" => gone("episode", id),
        Item::Movie(id) => match lib.get_movie(id).await {
            Ok(m) => vec![message("movie", action, super::radarr::movie_json(&m))],
            Err(_) => Vec::new(),
        },
        Item::Series(id) => match lib.get_series(id).await {
            Ok(s) => vec![message("series", action, super::sonarr::series_json(&s))],
            Err(_) => Vec::new(),
        },
        Item::Episode(id) => episode_messages(&lib, &[id], action).await,
        Item::MovieFile(file) => match lib.movie_of_file(file).await.ok().flatten() {
            Some(id) => match lib.get_movie(id).await {
                Ok(m) => vec![message("movie", "updated", super::radarr::movie_json(&m))],
                Err(_) => Vec::new(),
            },
            None => Vec::new(),
        },
        Item::EpisodeFile(file) => {
            let ids = lib.episodes_of_file(file).await.unwrap_or_default();
            episode_messages(&lib, &ids, "updated").await
        }
    }
}

/// Messages for episodes by id, each with its series, as Sonarr sends them.
async fn episode_messages(lib: &reelix_library::LibraryService, ids: &[i64], action: &str) -> Vec<Value> {
    let Some(&first) = ids.first() else {
        return Vec::new();
    };
    let Some(series_id) = lib.series_of_episode(first).await.ok().flatten() else {
        return Vec::new();
    };
    let (Ok(s), Ok(episodes)) = (
        lib.get_series(series_id).await,
        lib.episodes(series_id, None).await,
    ) else {
        return Vec::new();
    };
    episodes
        .iter()
        .filter(|e| ids.contains(&e.id))
        .map(|e| message("episode", action, super::sonarr::episode_json(e, Some(&s))))
        .collect()
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
async fn hub(State(state): State<AppState>, req: Request, which: Hub) -> Response {
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
    upgrade.on_upgrade(move |socket| serve(socket, state, key, user, which))
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

async fn serve(mut socket: WebSocket, state: AppState, key: String, user: User, which: Hub) {
    tracing::debug!(user = %user.username, hub = ?which, "SignalR hub connected");
    // Subscribed before the handshake, so no change made meanwhile is missed.
    let mut changes = state.events().subscribe();
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
    let mut flush = tokio::time::interval(FLUSH_EVERY);
    flush.reset();
    let mut pending: BTreeMap<Item, &'static str> = BTreeMap::new();
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) if closes(t.as_str()) => break,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                // Pings and anything else the client sends need no answer.
                Some(Ok(_)) => {}
            },
            change = changes.recv() => match change {
                Ok(event) => {
                    if let Some((item, action)) = wanted(which, &event) {
                        gather(&mut pending, item, action);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(missed = n, "SignalR hub fell behind the library's changes");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            _ = flush.tick(), if !pending.is_empty() => {
                let mut batch = std::mem::take(&mut pending);
                if batch.len() > MAX_BATCH {
                    tracing::debug!(changes = batch.len(), "SignalR hub: many changes at once; updates left to the client's sync");
                    batch.retain(|_, action| *action != "updated");
                }
                let mut sent = true;
                for (item, action) in batch {
                    for m in messages_for(&state, item, action).await {
                        if socket.send(record(&m)).await.is_err() {
                            sent = false;
                            break;
                        }
                    }
                    if !sent {
                        break;
                    }
                }
                if !sent {
                    break;
                }
            }
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

    fn changed(entity: &str, action: &str, id: i64) -> DomainEvent {
        DomainEvent::EntityChanged {
            entity: entity.into(),
            action: action.into(),
            id,
            user_id: None,
        }
    }

    #[test]
    fn each_hub_sends_its_own_apps_changes() {
        let r = |e, a, id| wanted(Hub::Radarr, &changed(e, a, id));
        let s = |e, a, id| wanted(Hub::Sonarr, &changed(e, a, id));
        assert_eq!(r("movie", "created", 1), Some((Item::Movie(1), "created")));
        assert_eq!(r("movie", "deleted", 1), Some((Item::Movie(1), "deleted")));
        assert_eq!(
            r("movie_file", "created", 7),
            Some((Item::MovieFile(7), "updated"))
        );
        assert_eq!(r("movie_file", "deleted", 7), None);
        assert_eq!(r("series", "updated", 2), None);
        assert_eq!(s("series", "updated", 2), Some((Item::Series(2), "updated")));
        assert_eq!(s("episode", "created", 3), Some((Item::Episode(3), "created")));
        assert_eq!(
            s("episode_file", "created", 9),
            Some((Item::EpisodeFile(9), "updated"))
        );
        assert_eq!(s("movie", "updated", 1), None);
        assert_eq!(s("queue", "updated", 1), None);
        assert_eq!(r("movie", "grabbed", 1), None);
        assert_eq!(
            wanted(Hub::Radarr, &DomainEvent::TaskUpdated { kind: "x".into() }),
            None
        );
    }

    #[test]
    fn gathered_changes_send_each_item_once() {
        let mut p = BTreeMap::new();
        gather(&mut p, Item::Movie(1), "created");
        gather(&mut p, Item::Movie(1), "updated");
        gather(&mut p, Item::Movie(2), "updated");
        gather(&mut p, Item::Movie(2), "updated");
        assert_eq!(p.len(), 2);
        assert_eq!(p[&Item::Movie(1)], "created", "still new to the client");
        gather(&mut p, Item::Movie(1), "deleted");
        assert_eq!(p[&Item::Movie(1)], "deleted");
    }

    #[test]
    fn messages_have_the_apps_shape() {
        let m = message("movie", "updated", json!({"id": 5, "title": "Heat"}));
        assert_eq!(m["type"], 1);
        assert_eq!(m["target"], "receiveMessage");
        assert_eq!(m["arguments"][0]["name"], "movie");
        assert_eq!(m["arguments"][0]["body"]["action"], "updated");
        assert_eq!(m["arguments"][0]["body"]["resource"]["id"], 5);
    }

    #[test]
    fn close_messages_are_found_among_others() {
        assert!(closes("{\"type\":6}\u{1e}{\"type\":7}\u{1e}"));
        assert!(!closes("{\"type\":6}\u{1e}"));
    }
}
