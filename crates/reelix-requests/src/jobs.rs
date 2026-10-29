//! Background jobs: request status sync (15 min) and Plex watchlist sync (1 h).

use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::requests::CreateRequestDto;
use reelix_core::MediaKind;
use reelix_jobs::{JobContext, JobHandler, TaskDef};

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

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        let n = self.0.sync_request_statuses().await?;
        tracing::debug!(changed = n, "request status sync finished");
        Ok(())
    }
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
