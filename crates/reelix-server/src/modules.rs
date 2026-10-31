//! Wiring for domain modules (library, providers, requests, pipeline, …).
//!
//! Startup runs in three phases so services, job handlers and the API can reference each other:
//! 1. [`Modules::build`] creates every service (no job queue yet),
//! 2. [`Modules::job_handlers`] registers job handlers and scheduled tasks,
//! 3. [`Modules::attach`] hands services the job queue and registers them on the API state.
//!
//! Each domain adds one field and one short block per phase.

use std::path::PathBuf;

use reelix_api::AppState;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_jobs::{JobQueue, JobSystemBuilder};

/// Shared dependencies available to every module.
#[derive(Clone)]
pub struct Deps {
    pub db: Db,
    pub events: EventBus,
    pub secrets: SecretBox,
    pub http: reqwest::Client,
    pub data_dir: PathBuf,
    /// Resolved bootstrap configuration (paths, config file, server settings).
    pub config: crate::config::Config,
}

/// Every domain service, built once at startup.
pub struct Modules {
    #[allow(dead_code)]
    deps: Deps,
    pub providers: std::sync::Arc<reelix_providers::ProviderService>,
    pub library: std::sync::Arc<reelix_library::LibraryService>,
    pub books: std::sync::Arc<reelix_books::BooksService>,
    pub music: std::sync::Arc<reelix_music::MusicService>,
    pub requests: std::sync::Arc<reelix_requests::RequestService>,
    pub integrations: std::sync::Arc<crate::integrations::IntegrationsService>,
    pub search: std::sync::Arc<reelix_pipeline::search::SearchService>,
    pub activity: std::sync::Arc<reelix_pipeline::ActivityService>,
    pub maintenance: std::sync::Arc<crate::maintenance::MaintenanceService>,
}

impl Modules {
    pub async fn build(deps: Deps) -> anyhow::Result<Self> {
        // providers
        let providers = reelix_providers::ProviderService::new(
            deps.db.clone(),
            deps.secrets.clone(),
            deps.events.clone(),
            deps.http.clone(),
            reelix_providers::ProviderOptions::new(&deps.data_dir),
        )
        .await?;
        // library (TMDB resolved per call from the providers' metadata settings)
        let library = {
            let p = providers.clone();
            let metadata = reelix_library::DynMetadata::new(move || {
                let p = p.clone();
                async move {
                    p.metadata()
                        .map(|t| t as std::sync::Arc<dyn reelix_library::MetadataSource>)
                        .map_err(|_| reelix_library::MetaError::NotConfigured("TMDB"))
                }
            });
            std::sync::Arc::new(reelix_library::LibraryService::new(
                deps.db.clone(),
                deps.events.clone(),
                std::sync::Arc::new(metadata),
            ))
        };
        // books (Open Library metadata, no API key); the client is shared with book Discover rows
        let open_library = reelix_metadata::OpenLibrary::new(deps.http.clone(), Default::default());
        let books = std::sync::Arc::new(reelix_books::BooksService::new(reelix_books::BooksDeps {
            db: deps.db.clone(),
            events: deps.events.clone(),
            http: deps.http.clone(),
            library: library.clone(),
            metadata: std::sync::Arc::new(open_library.clone()),
        }));
        // music (MusicBrainz + Cover Art Archive metadata, no API key)
        let music = std::sync::Arc::new(reelix_music::MusicService::new(reelix_music::MusicDeps {
            db: deps.db.clone(),
            events: deps.events.clone(),
            http: deps.http.clone(),
            library: library.clone(),
            metadata: std::sync::Arc::new(reelix_metadata::MusicBrainz::new(
                deps.http.clone(),
                Default::default(),
            )),
        }));
        // requests (metadata, media servers and notifications via providers; library via LibraryAdapter)
        let requests = {
            use reelix_requests::gateway::{ResolvingSource, adapters};
            use reelix_requests::mediaserver::{BuildContext, plextv::PlexTv};
            use std::sync::Arc;
            let client_id = deps
                .db
                .settings()
                .get::<String>("providers.client_id")
                .await?
                .unwrap_or_else(|| "reelix".into());
            let p = providers.clone();
            let metadata = ResolvingSource(Arc::new(move || {
                p.metadata()
                    .map(|t| t as Arc<dyn reelix_requests::DiscoverySource>)
                    .map_err(|_| reelix_requests::metadata::MetaError::NotConfigured("TMDB"))
            }));
            let p = providers.clone();
            let media_servers = adapters::FnDirectory {
                servers: Arc::new(move || {
                    p.media_servers()
                        .into_iter()
                        .filter(|(row, _)| row.enabled)
                        .filter_map(|(row, server)| {
                            let kind = match row.implementation.as_str() {
                                "jellyfin" => reelix_requests::MediaServerKind::Jellyfin,
                                "emby" => reelix_requests::MediaServerKind::Emby,
                                "plex" => reelix_requests::MediaServerKind::Plex,
                                _ => return None,
                            };
                            Some(reelix_requests::MediaServerEntry {
                                id: row.id,
                                name: row.name,
                                kind,
                                server,
                            })
                        })
                        .collect()
                }),
                plex_tv: PlexTv::new(&BuildContext::new(deps.http.clone(), client_id)),
            };
            let p = providers.clone();
            let notifications = adapters::FnNotifications(Arc::new(move |event, _recipients| {
                let p = p.clone();
                Box::pin(async move { p.notify(&event).await })
            }));
            Arc::new(reelix_requests::RequestService::new(
                reelix_requests::RequestDeps {
                    db: deps.db.clone(),
                    events: deps.events.clone(),
                    secrets: deps.secrets.clone(),
                    library: Arc::new(crate::adapters::LibraryAdapter {
                        library: library.clone(),
                        db: deps.db.clone(),
                        books: books.clone(),
                        music: music.clone(),
                        open_library: open_library.clone(),
                        listenbrainz: reelix_metadata::ListenBrainz::new(
                            deps.http.clone(),
                            Default::default(),
                        ),
                    }),
                    media_servers: Arc::new(media_servers),
                    metadata: Arc::new(metadata),
                    notifications: Arc::new(notifications),
                    http: Some(deps.http.clone()),
                },
            ))
        };
        // integrations (import list sync, notification bridge, health watch, media server availability)
        let integrations = {
            let p = providers.clone();
            let metadata = reelix_library::DynMetadata::new(move || {
                let p = p.clone();
                async move {
                    p.metadata()
                        .map(|t| t as std::sync::Arc<dyn reelix_library::MetadataSource>)
                        .map_err(|_| reelix_library::MetaError::NotConfigured("TMDB"))
                }
            });
            crate::integrations::IntegrationsService::new(
                deps.db.clone(),
                deps.events.clone(),
                library.clone(),
                std::sync::Arc::new(metadata),
                crate::integrations::Sources::from_providers(&providers),
            )
        };
        // pipeline: search / grab / RSS / delayed releases
        let search = std::sync::Arc::new(reelix_pipeline::search::SearchService::new(
            deps.db.clone(),
            deps.events.clone(),
            providers.clone(),
            library.clone(),
            reelix_pipeline::search::SearchOptions::default(),
        ));
        // pipeline: download monitoring, import, queue/history, manual import
        let activity = std::sync::Arc::new(reelix_pipeline::ActivityService::new(
            deps.db.clone(),
            deps.events.clone(),
            library.clone(),
            providers.clone(),
        ));
        // books / music hand-offs (search, import, music import lists)
        search.set_content(books.clone(), music.clone());
        activity.set_content(books.clone(), music.clone());
        integrations.set_music(music.clone());
        // maintenance: backups, logs, update checks, server settings
        let maintenance =
            crate::maintenance::MaintenanceService::new(deps.db.clone(), deps.http.clone(), &deps.config)
                .await?;
        Ok(Self {
            deps,
            providers,
            library,
            books,
            music,
            requests,
            integrations,
            search,
            activity,
            maintenance,
        })
    }

    pub fn job_handlers(&self, builder: JobSystemBuilder) -> JobSystemBuilder {
        let mut builder = builder;
        // providers
        builder = builder
            .handler(reelix_providers::jobs::IndexerHealthJob(self.providers.clone()))
            .handler(reelix_providers::jobs::ClientHealthJob(self.providers.clone()))
            .handler(reelix_providers::jobs::DefinitionsSyncJob(self.providers.clone()));
        for t in reelix_providers::jobs::tasks() {
            builder = builder.task(t);
        }
        // library
        builder = reelix_library::jobs::register(builder, &self.library);
        // books / music
        builder = reelix_books::jobs::register(builder, &self.books);
        builder = reelix_music::jobs::register(builder, &self.music);
        // requests
        builder = builder
            .handler(reelix_requests::RequestStatusSyncJob(self.requests.clone()))
            .handler(reelix_requests::WatchlistSyncJob(self.requests.clone()));
        for t in reelix_requests::tasks() {
            builder = builder.task(t);
        }
        // integrations
        builder = crate::integrations::register(builder, &self.integrations);
        // pipeline search (search, rss_sync, process_pending)
        builder = reelix_pipeline::search::jobs::register(builder, &self.search);
        // pipeline activity (download_monitor, process_download, manual_import, prune_finished)
        builder = reelix_pipeline::monitor::jobs::register(builder, &self.activity);
        // maintenance (backup, update_check)
        builder = crate::maintenance::MaintenanceService::register(builder, &self.maintenance);
        builder
    }

    pub fn attach(&self, _queue: &JobQueue, state: AppState) -> AppState {
        let mut state = state;
        // providers
        state = state.with_extension(self.providers.clone());
        // library
        self.library.set_queue(_queue.clone());
        state = state.with_extension(self.library.clone());
        // books / music
        self.books.set_queue(_queue.clone());
        state = state.with_extension(self.books.clone());
        self.music.set_queue(_queue.clone());
        state = state.with_extension(self.music.clone());
        // requests
        self.requests.set_queue(_queue.clone());
        state = state.with_extension(self.requests.clone());
        // pipeline search
        state = state.with_extension(self.search.clone());
        // pipeline activity
        self.activity.set_queue(_queue.clone());
        state = state.with_extension(self.activity.clone());
        // Health sources: add other services' `HealthSource`s to this list.
        let health: Vec<std::sync::Arc<dyn reelix_api::HealthSource>> = vec![
            self.providers.clone(),
            std::sync::Arc::new(reelix_api::ActivityHealth(self.activity.clone())),
            self.maintenance.clone(),
        ];
        // integrations (health_watch uses the same sources)
        self.integrations.set_health_sources(health.clone());
        self.integrations.spawn_bridge();
        state = state.with_extension(std::sync::Arc::new(reelix_api::Integrations(
            self.integrations.clone(),
        )));
        state = state.with_extension(std::sync::Arc::new(reelix_api::HealthSources(health)));
        // maintenance
        state = state.with_extension(std::sync::Arc::new(reelix_api::Maintenance(
            self.maintenance.clone(),
        )));
        state
    }
}

/// HTTP client shared by outbound integrations (metadata, indexers, clients, notifications).
pub fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Reelix/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60))
        .build()?)
}
