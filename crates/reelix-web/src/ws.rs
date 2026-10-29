//! Live `DomainEvent` stream over the `/api/v1/ws` WebSocket, with reconnect backoff.

use std::collections::VecDeque;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::channel::oneshot;
use futures::future::{Either, select};
use futures::{FutureExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::{Message, State};
use gloo_timers::future::sleep;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::DomainEvent;

use crate::api;

/// How many recent events are kept in memory.
pub const MAX_EVENTS: usize = 50;
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsStatus {
    /// Not signed in, so no connection is wanted.
    Idle,
    Connecting,
    Connected,
    /// Lost the connection; a retry is scheduled.
    Reconnecting,
}

impl WsStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Offline",
            Self::Connecting => "Connecting…",
            Self::Connected => "Live",
            Self::Reconnecting => "Reconnecting…",
        }
    }
}

/// An event plus when the UI received it.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveEvent {
    /// Monotonic sequence number, unique per page load.
    pub seq: u64,
    pub received_at: DateTime<Utc>,
    pub event: DomainEvent,
}

#[derive(Clone, Copy)]
pub struct LiveEvents {
    pub status: RwSignal<WsStatus>,
    /// Newest first, at most [`MAX_EVENTS`].
    pub events: RwSignal<VecDeque<LiveEvent>>,
    stop: StoredValue<Option<oneshot::Sender<()>>>,
    seq: StoredValue<u64>,
}

impl LiveEvents {
    pub fn new() -> Self {
        Self {
            status: RwSignal::new(WsStatus::Idle),
            events: RwSignal::new(VecDeque::new()),
            stop: StoredValue::new(None),
            seq: StoredValue::new(0),
        }
    }

    pub fn expect() -> Self {
        expect_context()
    }

    /// The most recent event, tracked reactively.
    pub fn latest(&self) -> Option<LiveEvent> {
        self.events.with(|e| e.front().cloned())
    }

    /// Opens the connection (no-op if already running).
    pub fn start(&self) {
        if self.stop.with_value(Option::is_some) {
            return;
        }
        let (tx, rx) = oneshot::channel();
        self.stop.set_value(Some(tx));
        let this = *self;
        spawn_local(async move { this.run(rx).await });
    }

    /// Closes the connection and stops reconnecting.
    pub fn stop(&self) {
        if let Some(tx) = self.stop.try_update_value(Option::take).flatten() {
            let _ = tx.send(());
        }
        if self
            .status
            .try_get_untracked()
            .is_some_and(|s| s != WsStatus::Idle)
        {
            self.status.set(WsStatus::Idle);
        }
    }

    fn record(&self, event: DomainEvent) {
        let seq = self.seq.get_value() + 1;
        self.seq.set_value(seq);
        self.events.update(|events| {
            events.push_front(LiveEvent {
                seq,
                received_at: Utc::now(),
                event,
            });
            events.truncate(MAX_EVENTS);
        });
    }

    async fn run(self, mut stop: oneshot::Receiver<()>) {
        let mut backoff = MIN_BACKOFF;
        loop {
            self.status.set(WsStatus::Connecting);
            match WebSocket::open(&api::ws_url()) {
                Ok(mut socket) => {
                    if self.wait_open(&mut socket, &mut stop).await {
                        return;
                    }
                    if matches!(socket.state(), State::Open) {
                        self.status.set(WsStatus::Connected);
                        backoff = MIN_BACKOFF;
                        if self.pump(&mut socket, &mut stop).await {
                            return;
                        }
                    }
                }
                Err(err) => leptos::logging::warn!("websocket open failed: {err}"),
            }

            self.status.set(WsStatus::Reconnecting);
            match select(sleep(backoff).boxed_local(), &mut stop).await {
                Either::Left(_) => {}
                Either::Right(_) => return,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Waits until the socket leaves the `Connecting` state. Returns `true` when stopped.
    async fn wait_open(&self, socket: &mut WebSocket, stop: &mut oneshot::Receiver<()>) -> bool {
        use futures::SinkExt;
        let ready = futures::future::poll_fn(|cx| socket.poll_ready_unpin(cx));
        matches!(select(ready, stop).await, Either::Right(_))
    }

    /// Forwards messages until the socket closes. Returns `true` when stopped.
    async fn pump(&self, socket: &mut WebSocket, stop: &mut oneshot::Receiver<()>) -> bool {
        loop {
            match select(socket.next(), &mut *stop).await {
                Either::Right(_) => return true,
                Either::Left((Some(Ok(Message::Text(text))), _)) => {
                    match serde_json::from_str::<DomainEvent>(&text) {
                        Ok(event) => self.record(event),
                        // Newer servers may send event types this UI doesn't know yet.
                        Err(err) => leptos::logging::debug_warn!("ignoring websocket message: {err}"),
                    }
                }
                Either::Left((Some(Ok(Message::Bytes(_))), _)) => {}
                Either::Left((Some(Err(_)) | None, _)) => return false,
            }
        }
    }
}
