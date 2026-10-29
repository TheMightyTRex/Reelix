//! [`MusicService`]: the facade used by the API, jobs, requests, import lists and the pipeline.
//!
//! The public operations live next to their logic: artists in `artists.rs`, albums, releases
//! and tracks in `albums.rs`/`releases.rs`, metadata profiles in `profiles.rs`, refresh in
//! `refresh.rs`, disk scans in `scan.rs`, library import in `library_import.rs`, renaming in
//! `rename.rs`, settings and root folders in `settings.rs`/`root_folders.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::music::AlbumSearchRequest;
use reelix_core::bus::EventBus;
use reelix_core::{ContentKind, DomainEvent};
use reelix_db::{Db, NewJob};
use reelix_decision::QualityProfile;
use reelix_import::files::{PathGuard, PermissionSettings, RecycleBin, delete_permanently};
use reelix_import::naming::NamingConfig;
use reelix_jobs::JobQueue;
use reelix_library::LibraryService;
use reelix_metadata::MusicMetadataProvider;
use reelix_parser::QualityFamily;

use crate::error::{MusicError, Result};
use crate::import_api::MusicImportApi;
use crate::kinds;
use crate::search_api::MusicSearchApi;

/// Everything [`MusicService::new`] needs.
pub struct MusicDeps {
    pub db: Db,
    pub events: EventBus,
    pub http: reqwest::Client,
    /// Naming, media management and root-folder helpers are shared with the library.
    pub library: Arc<LibraryService>,
    pub metadata: Arc<dyn MusicMetadataProvider>,
}

/// Artists, albums, releases, tracks, track files, metadata profiles and music root folders.
pub struct MusicService {
    pub(crate) db: Db,
    pub(crate) events: EventBus,
    pub(crate) http: reqwest::Client,
    pub(crate) library: Arc<LibraryService>,
    pub(crate) metadata: Arc<dyn MusicMetadataProvider>,
    pub(crate) queue: OnceLock<JobQueue>,
    /// Override "today" (tests).
    pub(crate) today_override: OnceLock<NaiveDate>,
}

/// Outcome of a disk scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub added: u32,
    pub updated: u32,
    pub removed: u32,
}

impl std::ops::AddAssign for ScanSummary {
    fn add_assign(&mut self, o: Self) {
        self.added += o.added;
        self.updated += o.updated;
        self.removed += o.removed;
    }
}

/// Music quality profiles + custom format names, for file DTOs and cutoffs.
pub(crate) struct ProfileCache {
    pub profiles: HashMap<i64, QualityProfile>,
    pub format_names: HashMap<i64, String>,
}

impl ProfileCache {
    pub fn profile(&self, id: i64) -> Option<&QualityProfile> {
        self.profiles.get(&id)
    }
}

impl MusicService {
    pub fn new(deps: MusicDeps) -> Self {
        Self {
            db: deps.db,
            events: deps.events,
            http: deps.http,
            library: deps.library,
            metadata: deps.metadata,
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

    /// Music profiles (every id resolvable through `get_any`) and custom format names.
    pub(crate) async fn profiles(&self) -> Result<ProfileCache> {
        let profiles = self
            .db
            .quality_profiles()
            .list_family(QualityFamily::Music)
            .await?;
        let formats = self.db.custom_formats().list().await?;
        Ok(ProfileCache {
            profiles: profiles.into_iter().map(|p| (p.id, p)).collect(),
            format_names: formats.iter().map(|f| (f.id, f.name.clone())).collect(),
        })
    }

    /// The artist's quality profile (any family id resolves; non-music ids are rejected on save).
    pub(crate) async fn quality_profile(&self, id: i64) -> Result<Option<QualityProfile>> {
        Ok(self.db.quality_profiles().get_any(id).await?)
    }

    pub(crate) async fn check_quality_profile(&self, id: i64) -> Result<()> {
        match self.quality_profile(id).await? {
            Some(p) if p.family == QualityFamily::Music => Ok(()),
            Some(p) => Err(MusicError::Validation(format!(
                "quality profile {id} ({}) is not a music profile",
                p.name
            ))),
            None => Err(MusicError::Validation(format!(
                "quality profile {id} does not exist"
            ))),
        }
    }

    /// Naming config of the library (colon replacement, illegal characters, lengths).
    pub(crate) async fn naming_config(&self) -> Result<NamingConfig> {
        let s = self.library.naming().await?;
        Ok(serde_json::to_value(&s)
            .and_then(serde_json::from_value)
            .unwrap_or_default())
    }

    /// Unix permissions from the library's media-management settings.
    pub(crate) async fn permissions(&self) -> Result<PermissionSettings> {
        let mm = self.library.media_management().await?;
        if !mm.set_permissions {
            return Ok(PermissionSettings::default());
        }
        let oct = |s: &Option<String>| s.as_deref().and_then(|m| u32::from_str_radix(m.trim(), 8).ok());
        Ok(PermissionSettings {
            file_mode: oct(&mm.file_mode),
            dir_mode: oct(&mm.dir_mode),
            uid: mm.chown_uid,
            gid: mm.chown_gid,
        })
    }

    /// Guard over every music root folder that exists on disk.
    pub(crate) async fn path_guard(&self) -> Result<PathGuard> {
        let mut g = PathGuard::default();
        for r in reelix_library::content_roots::list(&self.db, &[ContentKind::Music]).await? {
            if Path::new(&r.path).is_dir() {
                g.add_root(Path::new(&r.path))?;
            }
        }
        Ok(g)
    }

    /// Deletes a file/folder inside a music root: into the recycle bin when configured, else
    /// permanently. Missing paths are ignored. Returns the recycle-bin location, if any.
    pub(crate) async fn remove_path(&self, path: &Path) -> Result<Option<PathBuf>> {
        if std::fs::symlink_metadata(path).is_err() {
            return Ok(None);
        }
        let guard = self.path_guard().await?;
        let mm = self.library.media_management().await?;
        let path = path.to_path_buf();
        let bin = mm.recycle_bin_path.clone();
        let days = mm.recycle_bin_retention_days;
        tokio::task::spawn_blocking(move || -> Result<Option<PathBuf>> {
            match bin.as_deref() {
                Some(bin) => {
                    let retention = (days > 0).then(|| Duration::from_secs(u64::from(days) * 86_400));
                    let bin = RecycleBin::new(bin, retention)?;
                    Ok(Some(bin.recycle(&path, &guard)?))
                }
                None => {
                    delete_permanently(&path, &guard)?;
                    Ok(None)
                }
            }
        })
        .await
        .map_err(|e| MusicError::Internal(e.to_string()))?
    }

    /// Removes empty folders from `dir` up to (not including) `stop`.
    pub(crate) fn prune_empty_dirs(dir: &Path, stop: &Path) {
        let mut cur = Some(dir.to_path_buf());
        while let Some(d) = cur {
            if d == stop || !d.starts_with(stop) {
                break;
            }
            if std::fs::remove_dir(&d).is_err() {
                break;
            }
            cur = d.parent().map(Path::to_path_buf);
        }
    }

    // ---------------- jobs / hand-offs ----------------

    pub async fn queue_refresh(&self, artist_ids: Vec<i64>) -> Result<Option<i64>> {
        let mut ids = artist_ids;
        ids.sort_unstable();
        let key = format!(
            "refresh_artists:{}",
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        );
        self.enqueue(
            NewJob::new(
                kinds::REFRESH_ARTISTS,
                serde_json::json!({ "artist_ids": ids, "manual": true }),
            )
            .dedupe(key),
        )
        .await
    }

    pub async fn queue_rescan(&self, artist_ids: Vec<i64>) -> Result<Option<i64>> {
        let mut ids = artist_ids;
        ids.sort_unstable();
        let key = format!(
            "music_scan:{}",
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        );
        self.enqueue(NewJob::new(kinds::MUSIC_SCAN, serde_json::json!({ "artist_ids": ids })).dedupe(key))
            .await
    }

    /// Enqueues an [`kinds::ALBUM_SEARCH`](crate::kinds::ALBUM_SEARCH) job for the pipeline.
    pub async fn request_search(&self, req: AlbumSearchRequest) -> Result<Option<i64>> {
        if matches!(req.kind, ContentSearchKind::Item | ContentSearchKind::Parent) && req.ids.is_empty() {
            return Err(MusicError::Validation("`ids` must not be empty".into()));
        }
        let mut ids = req.ids.clone();
        ids.sort_unstable();
        let dedupe = format!(
            "album_search:{}:{}",
            crate::convert::enum_str(&req.kind),
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        );
        let payload = serde_json::to_value(&req).map_err(|e| MusicError::Internal(e.to_string()))?;
        self.enqueue(NewJob::new(kinds::ALBUM_SEARCH, payload).dedupe(dedupe))
            .await
    }

    pub fn search_api(&self) -> MusicSearchApi<'_> {
        MusicSearchApi { svc: self }
    }

    pub fn import_api(&self) -> MusicImportApi<'_> {
        MusicImportApi { svc: self }
    }
}
