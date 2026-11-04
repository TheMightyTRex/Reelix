//! [`RadioService`]: the facade used by the API, jobs and (later) the library sync and filing.
//!
//! The public operations live next to their logic: shows, episodes, search and preview in
//! `shows.rs`, refresh in `refresh.rs`, manual matching in `matching.rs`, the podcast library
//! (sync seam, file rows, review) in `library.rs`, settings and root folders in
//! `settings.rs`/`root_folders.rs`.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use chrono::{DateTime, Utc};
use reelix_core::DomainEvent;
use reelix_core::bus::EventBus;
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewJob};
use reelix_jobs::JobQueue;

use crate::error::Result;
use crate::kinds;
use crate::library::{NoopLibrarySync, RadioLibrarySync};
use crate::provider::ProviderRegistry;

/// Everything [`RadioService::new`] needs.
pub struct RadioDeps {
    pub db: Db,
    pub events: EventBus,
    pub http: reqwest::Client,
    pub providers: ProviderRegistry,
    /// Reads the Audiobookshelf podcast library; `None` = [`NoopLibrarySync`] (can also be
    /// attached later with [`RadioService::set_library_sync`]).
    pub library_sync: Option<Arc<dyn RadioLibrarySync>>,
}

/// Radio shows, series, episodes, stations, episode files and radio root folders.
pub struct RadioService {
    pub(crate) db: Db,
    pub(crate) events: EventBus,
    pub(crate) http: reqwest::Client,
    pub(crate) providers: ProviderRegistry,
    pub(crate) library_sync: RwLock<Arc<dyn RadioLibrarySync>>,
    pub(crate) queue: OnceLock<JobQueue>,
    /// The data folder (downloaded artwork and logos go under `radio/`); unset = no downloads.
    pub(crate) data_dir: OnceLock<PathBuf>,
    /// Encrypts provider credentials in the settings; unset = credentials cannot be saved.
    pub(crate) secrets: OnceLock<SecretBox>,
    /// Override "now" (tests).
    pub(crate) now_override: RwLock<Option<DateTime<Utc>>>,
}

impl RadioService {
    pub fn new(deps: RadioDeps) -> Self {
        Self {
            db: deps.db,
            events: deps.events,
            http: deps.http,
            providers: deps.providers,
            library_sync: RwLock::new(deps.library_sync.unwrap_or_else(|| Arc::new(NoopLibrarySync))),
            queue: OnceLock::new(),
            data_dir: OnceLock::new(),
            secrets: OnceLock::new(),
            now_override: RwLock::new(None),
        }
    }

    /// Gives the service the job queue (server `attach` phase). Later calls are ignored.
    pub fn set_queue(&self, queue: JobQueue) {
        let _ = self.queue.set(queue);
    }

    /// Sets the data folder; artwork and logos are downloaded under `<data_dir>/radio/`
    /// (server `build` phase). Later calls are ignored.
    pub fn set_data_dir(&self, dir: PathBuf) {
        let _ = self.data_dir.set(dir);
    }

    /// Sets the key that encrypts provider credentials in the radio settings. Later calls are
    /// ignored.
    pub fn set_secrets(&self, secrets: SecretBox) {
        let _ = self.secrets.set(secrets);
    }

    /// Replaces the podcast library reader (the Audiobookshelf implementation).
    pub fn set_library_sync(&self, sync: Arc<dyn RadioLibrarySync>) {
        if let Ok(mut s) = self.library_sync.write() {
            *s = sync;
        }
    }

    pub(crate) fn library_sync(&self) -> Arc<dyn RadioLibrarySync> {
        self.library_sync
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| Arc::new(NoopLibrarySync))
    }

    /// Pins "now" for deterministic tests.
    #[doc(hidden)]
    pub fn set_now(&self, t: DateTime<Utc>) {
        if let Ok(mut n) = self.now_override.write() {
            *n = Some(t);
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The HTTP client shared with the providers (for artwork downloads and connectors).
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.now_override
            .read()
            .ok()
            .and_then(|n| *n)
            .unwrap_or_else(Utc::now)
    }

    /// Publishes `entity.changed` (`radio_show`, `radio_episode`, `radio_episode_file`, …).
    pub fn emit(&self, entity: &str, action: &str, id: i64) {
        self.events.publish(DomainEvent::EntityChanged {
            entity: entity.to_owned(),
            action: action.to_owned(),
            id,
            user_id: None,
        });
    }

    /// Enqueues a job; `Ok(None)` when deduplicated or when no queue is attached (tests).
    pub(crate) async fn enqueue(&self, job: NewJob) -> Result<Option<i64>> {
        match self.queue.get() {
            Some(q) => Ok(q.enqueue(job).await?),
            None => {
                tracing::debug!(kind = %job.kind, "no job queue attached; job dropped");
                Ok(None)
            }
        }
    }

    // ---------------- jobs / hand-offs ----------------

    /// Queues a refresh of `show_ids` (empty = every show).
    pub async fn queue_refresh(&self, show_ids: Vec<i64>) -> Result<Option<i64>> {
        let mut ids = show_ids;
        ids.sort_unstable();
        ids.dedup();
        let key = format!(
            "{}:{}",
            kinds::REFRESH_RADIO_SHOWS,
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        );
        self.enqueue(
            NewJob::new(
                kinds::REFRESH_RADIO_SHOWS,
                serde_json::json!({ "show_ids": ids, "manual": true }),
            )
            .dedupe(key),
        )
        .await
    }

    /// Queues a podcast library sync (`radio_library_sync`).
    pub async fn queue_library_sync(&self) -> Result<Option<i64>> {
        self.enqueue(
            NewJob::new(kinds::RADIO_LIBRARY_SYNC, serde_json::json!({})).dedupe(kinds::RADIO_LIBRARY_SYNC),
        )
        .await
    }
}
