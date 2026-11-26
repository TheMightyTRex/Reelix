//! [`LibraryService`]: the facade used by the API, jobs and (via adapters) requests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

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
    /// The parsed quality profiles and custom formats, with the [`Db::profiles_generation`] they were read at.
    profile_cache: Mutex<Option<(u64, Arc<ProfileCache>)>>,
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
            profile_cache: Mutex::new(None),
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

    /// The video quality profiles and custom formats. They are read and parsed once, then kept until a
    /// profile or format is created, changed, deleted or seeded (the database counts these in
    /// [`Db::profiles_generation`]); a refresh, scan or search used to read them again on every call.
    pub(crate) async fn profiles(&self) -> Result<Arc<ProfileCache>> {
        // Read the number before the profiles: a change made while they load makes the copy look old
        // at its next use, never new.
        let generation = self.db.profiles_generation();
        if let Some((g, cache)) = self
            .profile_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && *g == generation
        {
            return Ok(cache.clone());
        }
        let profiles = self.db.quality_profiles().list().await?;
        let formats = self.db.custom_formats().list().await?;
        let cache = Arc::new(ProfileCache {
            profiles: profiles.into_iter().map(|p| (p.id, p)).collect(),
            format_names: formats.iter().map(|f| (f.id, f.name.clone())).collect(),
            formats,
        });
        // The first read of a new database seeds the built-in profiles, which moves the number on; the copy
        // then looks old once and is read again, after which it is kept.
        *self.profile_cache.lock().unwrap_or_else(|p| p.into_inner()) = Some((generation, cache.clone()));
        Ok(cache)
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

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use reelix_core::MediaKind;
    use reelix_metadata::{
        CollectionMetadata, ExternalSource, MetaError, MovieMetadata, SearchHit, SeriesExternalId,
        SeriesMetadata,
    };

    use super::*;

    /// A metadata source that is never asked anything.
    struct NoMetadata;

    #[async_trait]
    impl MetadataSource for NoMetadata {
        async fn movie(&self, _: u32) -> Result<MovieMetadata, MetaError> {
            Err(MetaError::NotFound)
        }
        async fn series(&self, _: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
            Err(MetaError::NotFound)
        }
        async fn collection(&self, _: u32) -> Result<CollectionMetadata, MetaError> {
            Err(MetaError::NotFound)
        }
        async fn search(&self, _: &str, _: MediaKind, _: Option<i32>) -> Result<Vec<SearchHit>, MetaError> {
            Ok(vec![])
        }
        async fn find(&self, _: ExternalSource, _: &str, _: MediaKind) -> Result<Vec<SearchHit>, MetaError> {
            Ok(vec![])
        }
    }

    async fn service() -> (LibraryService, Db) {
        let db = Db::in_memory().await.unwrap();
        let svc = LibraryService::new(db.clone(), EventBus::default(), Arc::new(NoMetadata));
        (svc, db)
    }

    fn format(name: &str) -> CustomFormat {
        CustomFormat {
            id: 0,
            name: name.into(),
            include_in_rename: false,
            specifications: vec![],
        }
    }

    #[tokio::test]
    async fn profiles_are_read_once_and_again_after_every_kind_of_change() {
        let (svc, db) = service().await;
        // The first read seeds the built-in profiles; after that the same copy is handed out.
        svc.profiles().await.unwrap();
        let first = svc.profiles().await.unwrap();
        assert!(!first.profiles.is_empty());
        assert!(
            Arc::ptr_eq(&first, &svc.profiles().await.unwrap()),
            "kept while nothing changes"
        );

        // A read that goes round the cache (the API's reads) changes nothing either.
        db.quality_profiles().list().await.unwrap();
        db.custom_formats().list().await.unwrap();
        assert!(Arc::ptr_eq(&first, &svc.profiles().await.unwrap()));

        // Every change path gives a new copy that shows the change.
        let mut last = first;
        macro_rules! changed {
            ($what:expr) => {{
                let now = svc.profiles().await.unwrap();
                assert!(!Arc::ptr_eq(&last, &now), "{} was not noticed", $what);
                assert!(
                    Arc::ptr_eq(&now, &svc.profiles().await.unwrap()),
                    "{} is kept again",
                    $what
                );
                last = now.clone();
                now
            }};
        }

        // Quality profile: create, update, delete.
        let mut profile = last.profiles.values().next().unwrap().clone();
        profile.name = "Cache test".into();
        let created = db.quality_profiles().create(&profile).await.unwrap();
        assert!(changed!("create profile").profiles.contains_key(&created.id));

        let mut edited = created.clone();
        edited.name = "Cache test renamed".into();
        edited.cutoff_format_score = 123;
        db.quality_profiles().update(created.id, &edited).await.unwrap();
        let now = changed!("update profile");
        assert_eq!(now.profiles[&created.id].name, "Cache test renamed");
        assert_eq!(now.profiles[&created.id].cutoff_format_score, 123);

        // Custom format: create, update, delete (which also removes its score from profiles).
        let cf = db
            .custom_formats()
            .create(&format("Cache format"), None)
            .await
            .unwrap();
        let now = changed!("create format");
        assert_eq!(now.format_names[&cf.id], "Cache format");
        let mut scored = now.profiles[&created.id].clone();
        scored.format_items = vec![reelix_decision::FormatItem {
            format_id: cf.id,
            score: 50,
        }];
        db.quality_profiles().update(created.id, &scored).await.unwrap();
        assert_eq!(
            changed!("score a format").profiles[&created.id]
                .format_items
                .len(),
            1
        );

        let mut renamed = cf.clone();
        renamed.name = "Cache format renamed".into();
        db.custom_formats().update(cf.id, &renamed).await.unwrap();
        assert_eq!(
            changed!("update format").format_names[&cf.id],
            "Cache format renamed"
        );

        db.custom_formats().delete(cf.id).await.unwrap();
        let now = changed!("delete format");
        assert!(!now.format_names.contains_key(&cf.id));
        assert!(
            now.profiles[&created.id].format_items.is_empty(),
            "its score went with it"
        );

        // Built-in formats made at start count as creations.
        db.custom_formats().ensure_builtins().await.unwrap();
        assert!(
            changed!("built-in formats")
                .formats
                .iter()
                .any(|f| f.name == "BSL")
        );

        // Delete the profile.
        db.quality_profiles().delete(created.id).await.unwrap();
        assert!(!changed!("delete profile").profiles.contains_key(&created.id));
    }

    #[tokio::test]
    async fn a_clone_of_the_database_handle_shares_the_change_count() {
        let (svc, db) = service().await;
        let before = svc.profiles().await.unwrap();
        let other = db.clone();
        other
            .custom_formats()
            .create(&format("From a clone"), None)
            .await
            .unwrap();
        let after = svc.profiles().await.unwrap();
        assert!(after.formats.iter().any(|f| f.name == "From a clone"));
        assert!(!Arc::ptr_eq(&before, &after));
    }
}
