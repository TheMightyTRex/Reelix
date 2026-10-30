//! Background jobs: request status sync (15 min) and Plex watchlist sync (1 h).

use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::requests::CreateRequestDto;
use reelix_core::bus::EventBus;
use reelix_core::{DomainEvent, MediaKind};
use reelix_db::NewJob;
use reelix_jobs::{JobContext, JobHandler, JobQueue, TaskDef};

use crate::repo::RequestFilter;
use crate::{Error, RequestService, Result};

pub const REQUEST_STATUS_SYNC: &str = "request_status_sync";
pub const WATCHLIST_SYNC: &str = "watchlist_sync";

/// Scheduled tasks of this crate.
pub fn tasks() -> Vec<TaskDef> {
    vec![
        TaskDef {
            kind: REQUEST_STATUS_SYNC,
            name: "Request status sync",
            interval: Duration::from_secs(15 * 60),
            run_on_startup: false,
        },
        TaskDef {
            kind: WATCHLIST_SYNC,
            name: "Watchlist sync",
            interval: Duration::from_secs(3600),
            run_on_startup: false,
        },
    ]
}

pub struct RequestStatusSyncJob(pub Arc<RequestService>);

#[async_trait::async_trait]
impl JobHandler for RequestStatusSyncJob {
    fn kind(&self) -> &'static str {
        REQUEST_STATUS_SYNC
    }

    async fn run(&self, _ctx: &JobContext, payload: serde_json::Value) -> anyhow::Result<()> {
        // `{"content": true}` (queued by the library watch): book/album requests only.
        let n = if payload.get("content").and_then(serde_json::Value::as_bool) == Some(true) {
            self.0.sync_content_statuses().await?
        } else {
            self.0.sync_request_statuses().await?
        };
        tracing::debug!(changed = n, "request status sync finished");
        Ok(())
    }
}

/// Library changes that can change the availability of a book/album request.
const CONTENT_ENTITIES: &[&str] = &[
    "book",
    "book_file",
    "author",
    "album",
    "trackfile",
    "track_file",
    "artist",
    "queue",
];

/// Debounce for library change bursts (e.g. an album import emits one event per track).
const CONTENT_SYNC_QUIET: Duration = Duration::from_secs(3);
const CONTENT_SYNC_MAX_WAIT: Duration = Duration::from_secs(30);

/// Watches book/music library changes and queues a book/album request status sync shortly
/// after they settle, so requests turn `processing` / `available` without waiting for the
/// 15-minute task. Needs a tokio runtime; does nothing without one.
pub(crate) fn spawn_content_watch(events: &EventBus, queue: JobQueue) {
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        tracing::debug!("no runtime: book/album request status watch disabled");
        return;
    };
    let mut rx = events.subscribe();
    rt.spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        let relevant = |ev: &DomainEvent| {
            matches!(ev, DomainEvent::EntityChanged { entity, .. } if CONTENT_ENTITIES.contains(&entity.as_str()))
        };
        loop {
            // Wait for the first relevant change.
            match rx.recv().await {
                Ok(ev) if relevant(&ev) => {}
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
            // Let the burst settle.
            let started = tokio::time::Instant::now();
            loop {
                let quiet = tokio::time::sleep(CONTENT_SYNC_QUIET);
                tokio::select! {
                    ev = rx.recv() => match ev {
                        Err(RecvError::Closed) => return,
                        _ if started.elapsed() >= CONTENT_SYNC_MAX_WAIT => break,
                        _ => continue,
                    },
                    () = quiet => break,
                }
            }
            // A sync that is already queued/running deduplicates this one; retry a few times
            // so changes made during a running sync are not lost.
            for _ in 0..5 {
                let job = NewJob::new(REQUEST_STATUS_SYNC, serde_json::json!({ "content": true }))
                    .dedupe(format!("{REQUEST_STATUS_SYNC}:content"));
                match queue.enqueue(job).await {
                    Ok(Some(_)) => break,
                    Ok(None) => tokio::time::sleep(CONTENT_SYNC_QUIET).await,
                    Err(e) => {
                        tracing::warn!(error = %e, "could not queue the request status sync");
                        break;
                    }
                }
            }
        }
    });
}

pub struct WatchlistSyncJob(pub Arc<RequestService>);

#[async_trait::async_trait]
impl JobHandler for WatchlistSyncJob {
    fn kind(&self) -> &'static str {
        WATCHLIST_SYNC
    }

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        let n = self.0.sync_watchlists().await?;
        tracing::debug!(created = n, "watchlist sync finished");
        Ok(())
    }
}

impl RequestService {
    /// Turns Plex watchlist items of opted-in users into requests. Returns how many were created.
    pub async fn sync_watchlists(&self) -> Result<usize> {
        let plex = self.media_servers.plex_tv();
        let mut created = 0;
        for (user, prefs) in self.users_repo().watchlist_users().await? {
            let Some(enc) = &user.plex_token else { continue };
            let token = match self.secrets.decrypt(enc) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(user = user.id, error = %e, "cannot decrypt plex token");
                    continue;
                }
            };
            let items = match plex.watchlist(&token, true).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(user = user.id, error = %e, "plex watchlist fetch failed");
                    continue;
                }
            };
            let Some(actor) = self.db.users().get(user.id).await? else {
                continue;
            };
            let previous = self
                .requests_repo()
                .find(&RequestFilter {
                    user_id: Some(user.id),
                    ..Default::default()
                })
                .await?;
            for item in items {
                let wanted = match item.kind {
                    MediaKind::Movie => prefs.watchlist_sync_movies,
                    MediaKind::Series => prefs.watchlist_sync_series,
                };
                let Some(tmdb) = item.tmdb_id.and_then(|t| u32::try_from(t).ok()) else {
                    continue;
                };
                if !wanted || previous.iter().any(|r| r.kind == item.kind && r.tmdb_id == tmdb) {
                    continue;
                }
                let dto = CreateRequestDto {
                    kind: item.kind,
                    tmdb_id: tmdb,
                    seasons: None,
                    is_4k: false,
                    profile_id: None,
                    root_folder_id: None,
                    tags: Vec::new(),
                    user_id: None,
                };
                match self.create_request_from(&actor, dto, "watchlist").await {
                    Ok(_) => created += 1,
                    Err(
                        Error::Conflict(_) | Error::Forbidden(_) | Error::Validation(_) | Error::NotFound(_),
                    ) => {}
                    Err(e) => tracing::warn!(user = user.id, tmdb, error = %e, "watchlist request failed"),
                }
            }
        }
        Ok(created)
    }
}
