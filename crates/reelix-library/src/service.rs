//! [`LibraryService`]: the facade used by the API, jobs and (via adapters) requests.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use chrono::{NaiveDate, Utc};
use reelix_api_types::library::{LibrarySearchKind, SearchRequest};
use reelix_core::DomainEvent;
use reelix_core::bus::EventBus;
use reelix_db::{Db, NewJob};
use reelix_decision::{CustomFormat, QualityProfile};
use reelix_jobs::JobQueue;

use crate::error::{LibraryError, Result};
use crate::kinds;
use crate::metadata::MetadataSource;

/// Movies, series, files, collections and root folders.
pub struct LibraryService {
    pub(crate) db: Db,
    pub(crate) events: EventBus,
    pub(crate) metadata: Arc<dyn MetadataSource>,
    pub(crate) queue: OnceLock<JobQueue>,
    /// Override "today" (tests).
    pub(crate) today_override: OnceLock<NaiveDate>,
}

/// Quality profiles + custom formats snapshot used to score files and compute cutoffs.
pub(crate) struct ProfileCache {
    pub profiles: HashMap<i64, QualityProfile>,
    pub formats: Vec<CustomFormat>,
    pub format_names: HashMap<i64, String>,
}

impl ProfileCache {
    pub fn profile(&self, id: i64) -> Option<&QualityProfile> {
        self.profiles.get(&id)
    }
}

impl LibraryService {
    pub fn new(db: Db, events: EventBus, metadata: Arc<dyn MetadataSource>) -> Self {
        Self {
            db,
            events,
            metadata,
            queue: OnceLock::new(),
            today_override: OnceLock::new(),
        }
    }

    /// Gives the service the job queue (server `attach` phase). Later calls are ignored.
    pub fn set_queue(&self, queue: JobQueue) {
        let _ = self.queue.set(queue);
    }

    /// Pins "today" for deterministic tests.
    #[doc(hidden)]
    pub fn set_today(&self, d: NaiveDate) {
        let _ = self.today_override.set(d);
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub(crate) fn today(&self) -> NaiveDate {
        self.today_override
            .get()
            .copied()
            .unwrap_or_else(|| Utc::now().date_naive())
    }

    pub(crate) fn emit(&self, entity: &str, action: &str, id: i64) {
        self.events.publish(DomainEvent::EntityChanged {
            entity: entity.to_owned(),
            action: action.to_owned(),
            id,
            user_id: None,
        });
    }

    pub(crate) async fn profiles(&self) -> Result<ProfileCache> {
        let profiles = self.db.quality_profiles().list().await?;
        let formats = self.db.custom_formats().list().await?;
        Ok(ProfileCache {
            profiles: profiles.into_iter().map(|p| (p.id, p)).collect(),
            format_names: formats.iter().map(|f| (f.id, f.name.clone())).collect(),
            formats,
        })
    }

    pub(crate) async fn check_profile(&self, id: i64) -> Result<()> {
        if self.db.quality_profiles().get(id).await?.is_none() {
            return Err(LibraryError::validation(format!(
                "quality profile {id} does not exist"
            )));
        }
        Ok(())
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

    /// Hands a search to the pipeline: enqueues job kind `"search"` with `req` as payload.
    pub async fn request_search(&self, req: SearchRequest) -> Result<Option<i64>> {
        if req.kind == LibrarySearchKind::Season && req.season.is_none() {
            return Err(LibraryError::validation("season search needs `season`"));
        }
        if matches!(
            req.kind,
            LibrarySearchKind::Movie
                | LibrarySearchKind::Series
                | LibrarySearchKind::Season
                | LibrarySearchKind::Episode
        ) && req.ids.is_empty()
        {
            return Err(LibraryError::validation("`ids` must not be empty"));
        }
        let payload = serde_json::to_value(&req)?;
        let mut ids = req.ids.clone();
        ids.sort_unstable();
        let dedupe = format!(
            "search:{}:{}:{:?}:{:?}",
            crate::convert::enum_str(&req.kind),
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","),
            req.season,
            req.media
        );
        self.enqueue(NewJob::new(kinds::SEARCH, payload).dedupe(dedupe))
            .await
    }
}
