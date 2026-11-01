//! M7 wiring: import list sync, notification bridge, health watch and media server
//! availability sync; M9 music import lists ([`music_lists`]: artists/albums added through the
//! music service).
//!
//! [`IntegrationsService`] is registered on the API state as [`reelix_api::Integrations`]
//! (exclusions, sync-now, list status, availability). Jobs: `import_list_sync` (15 min; each
//! list re-syncs after its own `sync_interval_hours`), `health_watch` (5 min) and
//! `media_server_availability` (6 h).

pub mod availability;
pub mod bridge;
pub mod health;
pub mod lists;
pub mod music_lists;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use reelix_api::{ApiError, ApiResult, HealthSource, IntegrationsBackend};
use reelix_api_types::integrations::{
    CreateExclusionRequest, CreateMusicExclusionRequest, ImportListExclusionDto, ImportListStatusDto,
    MediaServerAvailabilityDto, MusicListExclusionDto,
};
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_jobs::{JobContext, JobHandler, TaskDef};
use reelix_library::{LibraryService, MetadataSource};
use reelix_lists::ImportList;
use reelix_mediaserver::MediaServer;
use reelix_notify::NotificationEvent;
use reelix_providers::{ProviderRow, ProviderService};

pub use lists::{ListOptions, ListSyncOutcome, SyncPayload};
pub use music_lists::{MusicLibrary, MusicListOptions};

pub const IMPORT_LIST_SYNC: &str = reelix_api::IMPORT_LIST_SYNC;
pub const HEALTH_WATCH: &str = "health_watch";
pub const MEDIA_SERVER_AVAILABILITY: &str = "media_server_availability";

type ListsFn = dyn Fn() -> Vec<(ProviderRow, Arc<dyn ImportList>)> + Send + Sync;
type ServersFn = dyn Fn() -> Vec<(ProviderRow, Arc<dyn MediaServer>)> + Send + Sync;
type NotifyFn = dyn Fn(NotificationEvent) -> BoxFuture<'static, ()> + Send + Sync;

/// Where live providers come from (the [`ProviderService`] in production, fakes in tests).
#[derive(Clone)]
pub struct Sources {
    pub import_lists: Arc<ListsFn>,
    pub media_servers: Arc<ServersFn>,
    pub notify: Arc<NotifyFn>,
}

impl Sources {
    pub fn from_providers(p: &Arc<ProviderService>) -> Self {
        let (a, b, c) = (p.clone(), p.clone(), p.clone());
        Self {
            import_lists: Arc::new(move || a.import_lists()),
            media_servers: Arc::new(move || b.media_servers()),
            notify: Arc::new(move |ev| {
                let p = c.clone();
                Box::pin(async move { p.notify(&ev).await })
            }),
        }
    }
}

/// Owns the M7 integration jobs and the API backend.
pub struct IntegrationsService {
    db: Db,
    events: EventBus,
    library: Arc<LibraryService>,
    metadata: Arc<dyn MetadataSource>,
    sources: Sources,
    health_sources: OnceLock<Vec<Arc<dyn HealthSource>>>,
    health: health::HealthWatch,
    server_timeout: Duration,
    /// Music import lists add artists/albums through the music service.
    music: OnceLock<Arc<dyn MusicLibrary>>,
}

impl IntegrationsService {
    pub fn new(
        db: Db,
        events: EventBus,
        library: Arc<LibraryService>,
        metadata: Arc<dyn MetadataSource>,
        sources: Sources,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            events,
            library,
            metadata,
            sources,
            health_sources: OnceLock::new(),
            health: health::HealthWatch::default(),
            server_timeout: Duration::from_secs(120),
            music: OnceLock::new(),
        })
    }

    /// Connects the music service (build phase). Later calls are ignored.
    pub fn set_music(&self, music: Arc<reelix_music::MusicService>) {
        let _ = self.music.set(music);
    }

    /// Connects any [`MusicLibrary`] (tests). Later calls are ignored.
    pub fn set_music_library(&self, music: Arc<dyn MusicLibrary>) {
        let _ = self.music.set(music);
    }

    /// Health sources watched by `health_watch` (set once in the attach phase).
    pub fn set_health_sources(&self, sources: Vec<Arc<dyn HealthSource>>) {
        let _ = self.health_sources.set(sources);
    }

    /// Starts the notification bridge on the event bus (call once, inside the runtime).
    pub fn spawn_bridge(&self) -> tokio::task::JoinHandle<()> {
        let rx = self.events.subscribe();
        let notify = self.sources.notify.clone();
        let bridge = bridge::Bridge::new(self.db.clone());
        // Book and music imports refresh the servers that serve that content kind.
        let refresher = bridge::LibraryRefresher::new(self.db.clone(), self.sources.media_servers.clone());
        tokio::spawn(refresher.run(self.events.subscribe()));
        tokio::spawn(bridge.run(rx, move |ev| notify(ev)))
    }

    /// Runs the health sources once and sends issue/restored notifications.
    pub async fn watch_health(&self) -> usize {
        let checks = self
            .health_sources
            .get()
            .map(|s| s.iter().flat_map(|s| s.checks()).collect())
            .unwrap_or_default();
        let events = self.health.update(checks);
        let n = events.len();
        for ev in events {
            (self.sources.notify)(ev).await;
        }
        n
    }

    /// Fetches every enabled media server's items and stores the matches.
    pub async fn sync_media_servers(&self) -> anyhow::Result<()> {
        let servers: Vec<_> = (self.sources.media_servers)()
            .into_iter()
            .filter(|(r, _)| r.enabled)
            // Book/music servers have no movie/series items to match.
            .filter(|(_, s)| availability::tracks_availability(s.as_ref()))
            .collect();
        let timeout = self.server_timeout;
        let results = futures::future::join_all(servers.iter().map(|(row, server)| async move {
            let items = match tokio::time::timeout(timeout, server.library_items()).await {
                Ok(Ok(items)) => Ok(items),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("timed out".to_owned()),
            };
            if let Err(e) = &items {
                tracing::warn!(server = %row.name, error = %e, "media server availability sync failed");
            }
            availability::ServerItems {
                server_id: row.id,
                items,
            }
        }))
        .await;
        availability::record(&self.db, results).await?;
        self.events.publish(reelix_core::DomainEvent::EntityChanged {
            entity: "media_server_availability".into(),
            action: "synced".into(),
            id: 0,
            user_id: None,
        });
        Ok(())
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(e)
}

impl IntegrationsBackend for IntegrationsService {
    fn exclusions(&self) -> BoxFuture<'_, ApiResult<Vec<ImportListExclusionDto>>> {
        Box::pin(async move { lists::list_exclusions(&self.db).await.map_err(internal) })
    }

    fn add_exclusion(&self, req: CreateExclusionRequest) -> BoxFuture<'_, ApiResult<ImportListExclusionDto>> {
        Box::pin(async move {
            lists::add_exclusion(&self.db, &req)
                .await
                .map_err(internal)?
                .ok_or_else(|| ApiError::Conflict("this item is already excluded".into()))
        })
    }

    fn delete_exclusions(&self, ids: Vec<i64>) -> BoxFuture<'_, ApiResult<u64>> {
        Box::pin(async move { lists::delete_exclusions(&self.db, &ids).await.map_err(internal) })
    }

    fn list_status(&self) -> BoxFuture<'_, ApiResult<Vec<ImportListStatusDto>>> {
        Box::pin(async move { lists::list_status(&self.db).await.map_err(internal) })
    }

    fn music_exclusions(&self) -> BoxFuture<'_, ApiResult<Vec<MusicListExclusionDto>>> {
        Box::pin(async move { music_lists::list_exclusions(&self.db).await.map_err(internal) })
    }

    fn add_music_exclusion(
        &self,
        req: CreateMusicExclusionRequest,
    ) -> BoxFuture<'_, ApiResult<MusicListExclusionDto>> {
        Box::pin(async move {
            music_lists::add_exclusion(&self.db, &req)
                .await
                .map_err(internal)?
                .ok_or_else(|| ApiError::Conflict("this artist / album is already excluded".into()))
        })
    }

    fn delete_music_exclusions(&self, ids: Vec<i64>) -> BoxFuture<'_, ApiResult<u64>> {
        Box::pin(async move {
            music_lists::delete_exclusions(&self.db, &ids)
                .await
                .map_err(internal)
        })
    }

    fn availability(&self) -> BoxFuture<'_, ApiResult<MediaServerAvailabilityDto>> {
        Box::pin(async move {
            let names: Vec<(i64, String)> = (self.sources.media_servers)()
                .into_iter()
                .map(|(r, _)| (r.id, r.name))
                .collect();
            availability::summary(&self.db, &names).await.map_err(internal)
        })
    }
}

// ---------------------------------------------------------------- jobs

pub struct ImportListSyncJob(pub Arc<IntegrationsService>);

#[async_trait::async_trait]
impl JobHandler for ImportListSyncJob {
    fn kind(&self) -> &'static str {
        IMPORT_LIST_SYNC
    }

    async fn run(&self, _ctx: &JobContext, payload: serde_json::Value) -> anyhow::Result<()> {
        let payload: SyncPayload = serde_json::from_value(payload).unwrap_or_default();
        let outcomes = self.0.sync_import_lists(&payload).await?;
        if !outcomes.is_empty() {
            let added: u32 = outcomes.iter().map(|o| o.added).sum();
            let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
            tracing::info!(lists = outcomes.len(), added, failed, "import list sync finished");
        }
        Ok(())
    }
}

pub struct HealthWatchJob(pub Arc<IntegrationsService>);

#[async_trait::async_trait]
impl JobHandler for HealthWatchJob {
    fn kind(&self) -> &'static str {
        HEALTH_WATCH
    }

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        self.0.watch_health().await;
        Ok(())
    }
}

pub struct MediaServerAvailabilityJob(pub Arc<IntegrationsService>);

#[async_trait::async_trait]
impl JobHandler for MediaServerAvailabilityJob {
    fn kind(&self) -> &'static str {
        MEDIA_SERVER_AVAILABILITY
    }

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        self.0.sync_media_servers().await
    }
}

/// Registers the handlers and scheduled tasks.
pub fn register(
    builder: reelix_jobs::JobSystemBuilder,
    svc: &Arc<IntegrationsService>,
) -> reelix_jobs::JobSystemBuilder {
    builder
        .long_running_handler(ImportListSyncJob(svc.clone()))
        .handler(HealthWatchJob(svc.clone()))
        .long_running_handler(MediaServerAvailabilityJob(svc.clone()))
        .task(TaskDef {
            kind: IMPORT_LIST_SYNC,
            name: "Import list sync",
            interval: Duration::from_secs(15 * 60),
            run_on_startup: false,
        })
        .task(TaskDef {
            kind: HEALTH_WATCH,
            name: "Health notifications",
            interval: Duration::from_secs(5 * 60),
            run_on_startup: false,
        })
        .task(TaskDef {
            kind: MEDIA_SERVER_AVAILABILITY,
            name: "Media server availability sync",
            interval: Duration::from_secs(6 * 3600),
            run_on_startup: false,
        })
}
