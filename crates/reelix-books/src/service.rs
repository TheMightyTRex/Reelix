//! [`BooksService`]: the facade used by the API, jobs, requests and the pipeline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use reelix_api_types::books::{AuthorDto, BookDto, BookSearchRequest};
use reelix_api_types::content::ContentSearchKind;
use reelix_core::bus::EventBus;
use reelix_core::{BookFormat, ContentKind, DomainEvent};
use reelix_db::{Db, NewJob};
use reelix_decision::{CustomFormat, QualityProfile};
use reelix_import::files::{PathGuard, RecycleBin, delete_permanently};
use reelix_import::naming::NamingConfig;
use reelix_jobs::JobQueue;
use reelix_library::LibraryService;
use reelix_library::content_roots::{self, ContentRoot};
use reelix_metadata::BookMetadataProvider;
use reelix_parser::QualityFamily;

use crate::error::{BooksError, Result};
use crate::import_api::BookImportApi;
use crate::kinds;
use crate::repo::books::BookFilter;
use crate::repo::{authors as author_repo, books as book_repo, files};
use crate::search_api::BookSearchApi;

/// Everything [`BooksService::new`] needs.
pub struct BooksDeps {
    pub db: Db,
    pub events: EventBus,
    pub http: reqwest::Client,
    /// Naming, media management and root-folder helpers are shared with the library.
    pub library: Arc<LibraryService>,
    pub metadata: Arc<dyn BookMetadataProvider>,
}

/// Authors, books, editions, series, files and book root folders.
pub struct BooksService {
    pub(crate) db: Db,
    pub(crate) events: EventBus,
    pub(crate) http: reqwest::Client,
    pub(crate) library: Arc<LibraryService>,
    pub(crate) metadata: Arc<dyn BookMetadataProvider>,
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

/// Ebook/audiobook quality profiles + custom formats.
pub(crate) struct BookProfiles {
    pub profiles: HashMap<i64, QualityProfile>,
    pub formats: Vec<CustomFormat>,
    pub format_names: HashMap<i64, String>,
}

impl BookProfiles {
    pub fn profile(&self, id: Option<i64>) -> Option<&QualityProfile> {
        id.and_then(|id| self.profiles.get(&id))
    }
}

/// The profile id of `author` for `format`.
pub(crate) fn profile_of(a: &AuthorDto, format: BookFormat) -> Option<i64> {
    match format {
        BookFormat::Ebook => a.quality_profile_id,
        BookFormat::Audiobook => a.audio_quality_profile_id,
    }
}

/// The folder of `author` for `format`.
pub(crate) fn path_of(a: &AuthorDto, format: BookFormat) -> Option<&str> {
    match format {
        BookFormat::Ebook => a.path.as_deref(),
        BookFormat::Audiobook => a.audio_path.as_deref(),
    }
    .filter(|p| !p.is_empty())
}

pub(crate) fn monitored_for(b: &BookDto, format: BookFormat) -> bool {
    match format {
        BookFormat::Ebook => b.monitored_ebook,
        BookFormat::Audiobook => b.monitored_audiobook,
    }
}

pub(crate) fn file_for(b: &BookDto, format: BookFormat) -> Option<&reelix_api_types::books::BookFileDto> {
    match format {
        BookFormat::Ebook => b.ebook_file.as_ref(),
        BookFormat::Audiobook => b.audiobook_file.as_ref(),
    }
}

impl BooksService {
    pub fn new(deps: BooksDeps) -> Self {
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

    pub(crate) async fn profiles(&self) -> Result<BookProfiles> {
        let profiles = self.db.quality_profiles().list_all().await?;
        let formats = self.db.custom_formats().list().await?;
        Ok(BookProfiles {
            profiles: profiles
                .into_iter()
                .filter(|p| matches!(p.family, QualityFamily::Ebook | QualityFamily::Audiobook))
                .map(|p| (p.id, p))
                .collect(),
            format_names: formats.iter().map(|f| (f.id, f.name.clone())).collect(),
            formats,
        })
    }

    /// Fails with `Validation` unless `id` is a quality profile of the format's family.
    pub(crate) async fn check_profile(&self, id: i64, format: BookFormat) -> Result<()> {
        match self.db.quality_profiles().get_any(id).await? {
            Some(p) if p.family == QualityFamily::of_book(format) => Ok(()),
            Some(_) => Err(BooksError::Validation(format!(
                "quality profile {id} is not an {} profile",
                format.as_str()
            ))),
            None => Err(BooksError::Validation(format!(
                "quality profile {id} does not exist"
            ))),
        }
    }

    /// The book root folder `id` of the format's kind.
    pub(crate) async fn require_root(&self, id: i64, format: BookFormat) -> Result<ContentRoot> {
        match content_roots::get(&self.db, id).await? {
            Some(r) if r.kind == format.content_kind() => Ok(r),
            Some(_) => Err(BooksError::Validation(format!(
                "root folder {id} is not an {} root folder",
                format.as_str()
            ))),
            None => Err(BooksError::Validation(format!("root folder {id} does not exist"))),
        }
    }

    /// Naming config of the library (colon replacement, illegal characters, length limits).
    pub(crate) async fn naming_config(&self) -> Result<NamingConfig> {
        let s = self.library.naming().await?;
        Ok(serde_json::to_value(&s)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    /// Guard over every book root folder that exists on disk.
    pub(crate) async fn path_guard(&self) -> Result<PathGuard> {
        let mut g = PathGuard::default();
        for r in content_roots::list(&self.db, &[ContentKind::Ebook, ContentKind::Audiobook]).await? {
            if Path::new(&r.path).is_dir() {
                g.add_root(Path::new(&r.path))?;
            }
        }
        Ok(g)
    }

    /// Deletes a file/folder inside the book roots: into the recycle bin when configured, else
    /// permanently. Missing paths are ignored. Returns the recycle-bin location, if any.
    pub(crate) async fn remove_path(&self, path: &Path) -> Result<Option<PathBuf>> {
        if std::fs::symlink_metadata(path).is_err() {
            return Ok(None);
        }
        let guard = self.path_guard().await?;
        let mm = self.library.media_management().await?;
        match mm.recycle_bin_path.as_deref().filter(|p| !p.trim().is_empty()) {
            Some(bin) => {
                let retention = (mm.recycle_bin_retention_days > 0)
                    .then(|| Duration::from_secs(u64::from(mm.recycle_bin_retention_days) * 86_400));
                let bin = RecycleBin::new(bin, retention)?;
                Ok(Some(bin.recycle(path, &guard)?))
            }
            None => {
                delete_permanently(path, &guard)?;
                Ok(None)
            }
        }
    }

    pub(crate) async fn author_row(&self, id: i64) -> Result<AuthorDto> {
        author_repo::get(&self.db, id)
            .await?
            .ok_or_else(|| BooksError::NotFound(format!("author {id} not found")))
    }

    /// Books with their ebook/audiobook files attached.
    pub(crate) async fn load_books(&self, f: BookFilter<'_>) -> Result<Vec<BookDto>> {
        let mut books = book_repo::load(&self.db, f).await?;
        if books.is_empty() {
            return Ok(books);
        }
        let ids: Vec<i64> = books.iter().map(|b| b.id).collect();
        let mut author_ids: Vec<i64> = books.iter().map(|b| b.author_id).collect();
        author_ids.sort_unstable();
        author_ids.dedup();
        let authors: HashMap<i64, AuthorDto> = author_repo::load(&self.db, Some(&author_ids))
            .await?
            .into_iter()
            .map(|a| (a.id, a))
            .collect();
        let profiles = self.profiles().await?;
        let mut by_book: HashMap<(i64, BookFormat), files::FileRow> = HashMap::new();
        for f in files::list(&self.db, Some(&ids)).await? {
            by_book.insert((f.book_id, f.format), f);
        }
        for b in &mut books {
            let Some(a) = authors.get(&b.author_id) else {
                continue;
            };
            for format in BookFormat::ALL {
                if let Some(f) = by_book.remove(&(b.id, format)) {
                    let dto = f.to_dto(
                        path_of(a, format),
                        profiles.profile(profile_of(a, format)),
                        &profiles.format_names,
                    );
                    match format {
                        BookFormat::Ebook => b.ebook_file = Some(dto),
                        BookFormat::Audiobook => b.audiobook_file = Some(dto),
                    }
                }
            }
        }
        Ok(books)
    }

    pub(crate) async fn book_row(&self, id: i64) -> Result<BookDto> {
        self.load_books(BookFilter {
            ids: Some(&[id]),
            author_ids: None,
        })
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| BooksError::NotFound(format!("book {id} not found")))
    }

    // ---------------- jobs / hand-offs ----------------

    /// Queues a metadata refresh of the given authors (empty = every author).
    pub async fn queue_refresh(&self, author_ids: Vec<i64>) -> Result<Option<i64>> {
        for id in &author_ids {
            self.author_row(*id).await?;
        }
        let mut ids = author_ids;
        ids.sort_unstable();
        ids.dedup();
        let key = format!(
            "{}:{}",
            kinds::REFRESH_AUTHORS,
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        );
        let payload = if ids.is_empty() {
            serde_json::json!({ "manual": true })
        } else {
            serde_json::json!({ "author_ids": ids, "manual": true })
        };
        self.enqueue(
            NewJob::new(kinds::REFRESH_AUTHORS, payload)
                .priority(10)
                .dedupe(key),
        )
        .await
    }

    /// Queues a disk scan of the given authors (empty = every author).
    pub async fn queue_rescan(&self, author_ids: Vec<i64>) -> Result<Option<i64>> {
        for id in &author_ids {
            self.author_row(*id).await?;
        }
        self.queue_scan(&author_ids, None).await
    }

    /// Queues a disk scan; `apply_monitor` re-applies an [`AuthorMonitor`] afterwards
    /// (`missing`/`existing` depend on the files found).
    ///
    /// [`AuthorMonitor`]: reelix_api_types::books::AuthorMonitor
    pub(crate) async fn queue_scan(
        &self,
        author_ids: &[i64],
        apply_monitor: Option<&str>,
    ) -> Result<Option<i64>> {
        let mut ids = author_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let key = format!(
            "{}:{}:{}",
            kinds::BOOK_SCAN,
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","),
            apply_monitor.unwrap_or("")
        );
        self.enqueue(
            NewJob::new(
                kinds::BOOK_SCAN,
                serde_json::json!({ "author_ids": ids, "apply_monitor": apply_monitor }),
            )
            .priority(5)
            .dedupe(key),
        )
        .await
    }

    /// Enqueues a [`kinds::BOOK_SEARCH`](crate::kinds::BOOK_SEARCH) job for the pipeline.
    pub async fn request_search(&self, req: BookSearchRequest) -> Result<Option<i64>> {
        if matches!(req.kind, ContentSearchKind::Item | ContentSearchKind::Parent) && req.ids.is_empty() {
            return Err(BooksError::Validation("`ids` must not be empty".into()));
        }
        let payload = serde_json::to_value(&req).map_err(|e| BooksError::Internal(e.to_string()))?;
        let mut ids = req.ids.clone();
        ids.sort_unstable();
        let kind = match req.kind {
            ContentSearchKind::Item => "item",
            ContentSearchKind::Parent => "parent",
            ContentSearchKind::Missing => "missing",
            ContentSearchKind::Cutoff => "cutoff",
        };
        let dedupe = format!(
            "{}:{kind}:{}:{}",
            kinds::BOOK_SEARCH,
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","),
            req.format.map_or("all", BookFormat::as_str)
        );
        self.enqueue(NewJob::new(kinds::BOOK_SEARCH, payload).dedupe(dedupe))
            .await
    }

    // ---------------- pipeline support ----------------

    pub fn search_api(&self) -> BookSearchApi<'_> {
        BookSearchApi { svc: self }
    }

    pub fn import_api(&self) -> BookImportApi<'_> {
        BookImportApi { svc: self }
    }
}
