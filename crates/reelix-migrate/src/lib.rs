//! Migration from an existing *arr stack: reads Radarr, Sonarr, Prowlarr, Readarr, Lidarr or
//! Jellyseerr over its API and recreates its settings and library in Reelix.
//!
//! [`MigrateService::preview`] shows what would happen without changing anything;
//! [`MigrateService::start`] runs it in the background and [`MigrateService::status`] reports
//! progress. Items already in Reelix are reused, so a migration can be repeated safely (for
//! example after adding a path rewrite or a TMDB key).

mod engine;
mod quality;
pub mod source;

use std::sync::{Arc, Mutex};

use chrono::Utc;
use reelix_api_types::migrate::{
    MigrateConnection, MigratePlan, MigrateRunDto, MigrateRunRequest, MigrateState, SourceKind,
};
use reelix_books::BooksService;
use reelix_db::Db;
use reelix_library::LibraryService;
use reelix_music::MusicService;
use reelix_providers::ProviderService;

use crate::engine::{Engine, Mode};
pub use crate::source::{Snapshot, Source};

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    /// Bad connection details.
    #[error("{0}")]
    Input(String),
    /// The source couldn't be read.
    #[error("{0}")]
    Source(String),
    #[error("a migration is already running")]
    Busy,
    #[error("{0}")]
    Internal(String),
}

/// The Reelix services a migration writes to; absent modules are skipped.
#[derive(Clone)]
pub struct Targets {
    pub db: Db,
    pub providers: Arc<ProviderService>,
    pub library: Option<Arc<LibraryService>>,
    pub books: Option<Arc<BooksService>>,
    pub music: Option<Arc<MusicService>>,
    /// Movie and series metadata (TMDB), for request titles.
    pub metadata: Arc<dyn reelix_library::MetadataSource>,
    /// Whether movie/series metadata is configured (a TMDB key is set).
    pub metadata_ready: Arc<dyn Fn() -> bool + Send + Sync>,
}

pub struct MigrateService {
    targets: Targets,
    http: reqwest::Client,
    run: Arc<Mutex<Option<MigrateRunDto>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl MigrateService {
    pub fn new(targets: Targets, http: reqwest::Client) -> Self {
        Self {
            targets,
            http,
            run: Arc::new(Mutex::new(None)),
        }
    }

    fn source(&self, c: &MigrateConnection) -> Result<Source, MigrateError> {
        Source::new(c.source, &c.url, &c.api_key, self.http.clone())
    }

    /// Checks the connection and returns the source's version.
    pub async fn test(&self, c: &MigrateConnection) -> Result<String, MigrateError> {
        Ok(self.source(c)?.status().await?.version)
    }

    /// What a migration would do; nothing is changed.
    pub async fn preview(&self, c: &MigrateConnection) -> Result<MigratePlan, MigrateError> {
        let snap = self.source(c)?.snapshot().await?;
        let noop = |_: usize, _: &str| {};
        let mut engine = Engine::new(&self.targets, &snap, c, Mode::Plan, &noop);
        engine.run().await.map_err(MigrateError::Internal)?;
        let mut notes = engine.notes;
        notes.extend(source_notes(c.source));
        Ok(MigratePlan {
            source: c.source,
            version: snap.version.clone(),
            items: engine.items,
            notes,
        })
    }

    /// The current or last run.
    pub fn status(&self) -> Option<MigrateRunDto> {
        lock(&self.run).clone()
    }

    /// Starts a migration in the background.
    pub fn start(self: &Arc<Self>, req: MigrateRunRequest) -> Result<MigrateRunDto, MigrateError> {
        let source = self.source(&req.connection)?;
        let initial = {
            let mut run = lock(&self.run);
            if run.as_ref().is_some_and(|r| r.state == MigrateState::Running) {
                return Err(MigrateError::Busy);
            }
            let dto = MigrateRunDto {
                source: req.connection.source,
                state: MigrateState::Running,
                started_at: Utc::now(),
                finished_at: None,
                done: 0,
                total: 0,
                current: Some(format!("Reading {}", req.connection.source.label())),
                items: Vec::new(),
                error: None,
            };
            *run = Some(dto.clone());
            dto
        };
        let this = self.clone();
        tokio::spawn(async move {
            let result = this.execute(&source, &req).await;
            let mut run = lock(&this.run);
            if let Some(r) = run.as_mut() {
                r.finished_at = Some(Utc::now());
                r.current = None;
                match result {
                    Ok(items) => {
                        r.state = MigrateState::Finished;
                        r.done = r.total;
                        r.items = items;
                    }
                    Err(e) => {
                        r.state = MigrateState::Failed;
                        r.error = Some(e);
                    }
                }
            }
            tracing::info!(source = req.connection.source.as_str(), "migration finished");
        });
        Ok(initial)
    }

    async fn execute(
        &self,
        source: &Source,
        req: &MigrateRunRequest,
    ) -> Result<Vec<reelix_api_types::migrate::MigrateItem>, String> {
        let snap = source.snapshot().await.map_err(|e| e.to_string())?;
        let total = Engine::total(&snap);
        if let Some(r) = lock(&self.run).as_mut() {
            r.total = total;
        }
        let run = self.run.clone();
        let progress = move |done: usize, what: &str| {
            if let Some(r) = lock(&run).as_mut() {
                r.done = done.min(total);
                r.current = Some(what.to_owned());
            }
        };
        let mode = Mode::Run {
            categories: req.categories.clone(),
            search_missing: req.search_missing,
        };
        let mut engine = Engine::new(&self.targets, &snap, &req.connection, mode, &progress);
        engine.run().await?;
        Ok(engine.items)
    }
}

/// Fixed caveats per source.
fn source_notes(kind: SourceKind) -> Vec<String> {
    let mut notes = vec![
        "Custom formats, delay profiles, notifications, import lists and history aren't migrated.".to_owned(),
    ];
    match kind {
        SourceKind::Readarr => notes.push(
            "Readarr authors are matched on Open Library by name; authors with no exact match are skipped."
                .into(),
        ),
        SourceKind::Prowlarr => notes.push(
            "Prowlarr's apps, sync profiles and download clients aren't migrated. Indexers synced from Prowlarr into Radarr or Sonarr are skipped when migrating those apps."
                .into(),
        ),
        SourceKind::Jellyseerr => notes.push(
            "Jellyfin, Emby and Plex users keep signing in with their media server account; local users need a new password."
                .into(),
        ),
        _ => {}
    }
    notes
}
