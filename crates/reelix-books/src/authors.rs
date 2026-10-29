//! Authors: list/get/add/edit/delete and lookup.

use std::cmp::Ordering;
use std::path::Path;

use reelix_api_types::books::{
    AddAuthorRequest, AuthorDto, AuthorEdit, AuthorListQuery, AuthorLookupDto, AuthorMonitor,
    BookSearchRequest,
};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::library::{Paged, SortDir};
use reelix_core::BookFormat;
use reelix_library::content_roots::{self, ContentRoot};
use reelix_metadata::AuthorSearchHit;

use crate::BooksService;
use crate::error::{BooksError, Result};
use crate::metadata::author_from;
use crate::naming::author_folder;
use crate::refresh::WorkMonitor;
use crate::repo::{authors as repo, books as book_repo, series as series_repo};

/// Slices `items` into a page (`page_size` 0 = all).
pub(crate) fn paginate<T>(items: Vec<T>, page: Option<u32>, page_size: Option<u32>) -> Paged<T> {
    let total = items.len() as u64;
    let page = page.unwrap_or(1).max(1);
    let size = page_size.unwrap_or(0);
    let items = if size == 0 {
        items
    } else {
        items
            .into_iter()
            .skip(((page - 1) as usize).saturating_mul(size as usize))
            .take(size as usize)
            .collect()
    };
    Paged {
        items,
        total,
        page: if size == 0 { 1 } else { page },
        page_size: size,
    }
}

/// Explicit author folders (library import); `None` = rendered from the naming settings.
#[derive(Debug, Clone, Default)]
pub(crate) struct AuthorPaths {
    pub ebook: Option<String>,
    pub audio: Option<String>,
}

pub(crate) fn is_author_olid(s: &str) -> bool {
    s.len() > 3
        && s.starts_with("OL")
        && s.ends_with('A')
        && s[2..s.len() - 1].chars().all(|c| c.is_ascii_digit())
}

fn monitor_str(m: AuthorMonitor) -> Option<&'static str> {
    match m {
        AuthorMonitor::Missing => Some("missing"),
        AuthorMonitor::Existing => Some("existing"),
        _ => None,
    }
}

impl BooksService {
    pub async fn list_authors(&self, q: &AuthorListQuery) -> Result<Paged<AuthorDto>> {
        let mut items = repo::load(&self.db, None).await?;
        let needle = q
            .search
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        items.retain(|a| {
            needle
                .as_ref()
                .is_none_or(|n| a.name.to_lowercase().contains(n) || a.sort_name.to_lowercase().contains(n))
                && q.monitored.is_none_or(|m| a.monitored == m)
        });
        let sort = q.sort.as_deref().unwrap_or("name");
        items.sort_by(|a, b| {
            let o = match sort {
                "added" => a.added.cmp(&b.added),
                "books" => a.statistics.book_count.cmp(&b.statistics.book_count),
                "size" => a.statistics.size_on_disk.cmp(&b.statistics.size_on_disk),
                _ => Ordering::Equal,
            };
            o.then_with(|| a.sort_name.to_lowercase().cmp(&b.sort_name.to_lowercase()))
        });
        if q.dir == Some(SortDir::Desc) {
            items.reverse();
        }
        Ok(paginate(items, q.page, q.page_size))
    }

    pub async fn get_author(&self, id: i64) -> Result<AuthorDto> {
        self.author_row(id).await
    }

    pub async fn author_by_openlibrary(&self, olid: &str) -> Result<Option<AuthorDto>> {
        match repo::id_by_olid(&self.db, olid.trim()).await? {
            Some(id) => Ok(Some(self.author_row(id).await?)),
            None => Ok(None),
        }
    }

    /// Adds an author with every work from Open Library, monitored per `req.monitor`; queues a
    /// disk scan and (optionally) a search.
    pub async fn add_author(&self, req: AddAuthorRequest) -> Result<AuthorDto> {
        self.add_author_inner(req, AuthorPaths::default(), None, true)
            .await
    }

    /// Validates the format settings of a new author; returns the ebook and audiobook roots.
    async fn author_roots(
        &self,
        req: &AddAuthorRequest,
    ) -> Result<(Option<ContentRoot>, Option<ContentRoot>)> {
        if req.quality_profile_id.is_none() && req.audio_quality_profile_id.is_none() {
            return Err(BooksError::Validation(
                "an ebook or audiobook quality profile is required".into(),
            ));
        }
        let mut roots = [None, None];
        for (i, (format, profile, root)) in [
            (BookFormat::Ebook, req.quality_profile_id, req.root_folder_id),
            (
                BookFormat::Audiobook,
                req.audio_quality_profile_id,
                req.audio_root_folder_id,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let Some(p) = profile else { continue };
            self.check_profile(p, format).await?;
            let root = root.ok_or_else(|| {
                BooksError::Validation(format!("a root folder is required for {}s", format.as_str()))
            })?;
            roots[i] = Some(self.require_root(root, format).await?);
        }
        let [e, a] = roots;
        Ok((e, a))
    }

    pub(crate) async fn add_author_inner(
        &self,
        req: AddAuthorRequest,
        paths: AuthorPaths,
        added_by_request_id: Option<i64>,
        scan: bool,
    ) -> Result<AuthorDto> {
        let olid = req.openlibrary_id.trim().to_string();
        if olid.is_empty() {
            return Err(BooksError::Validation("openlibrary_id is required".into()));
        }
        let (ebook_root, audio_root) = self.author_roots(&req).await?;
        if repo::id_by_olid(&self.db, &olid).await?.is_some() {
            return Err(BooksError::Conflict(format!(
                "author {olid} is already in the library"
            )));
        }
        let meta = self.metadata.author(&olid).await?;
        let works = self.metadata.author_works(&olid).await?;
        let mut a = author_from(&meta);
        a.openlibrary_id = olid.clone();
        a.monitored = req.monitored;
        a.monitor_new_items = req.monitor_new_items;
        a.quality_profile_id = req.quality_profile_id;
        a.audio_quality_profile_id = req.audio_quality_profile_id;
        a.root_folder_id = ebook_root.as_ref().map(|r| r.id);
        a.audio_root_folder_id = audio_root.as_ref().map(|r| r.id);
        a.tags = req.tags.clone();
        a.tags.sort_unstable();
        a.tags.dedup();
        let settings = self.settings().await?;
        let config = self.naming_config().await?;
        let folder = author_folder(&settings.naming, &a, &config);
        let place = |root: &Option<ContentRoot>, explicit: Option<String>| {
            root.as_ref()
                .map(|r| match explicit.filter(|p| !p.trim().is_empty()) {
                    Some(p) => content_roots::normalize(&p),
                    None => Path::new(&r.path).join(&folder).to_string_lossy().into_owned(),
                })
        };
        a.path = place(&ebook_root, paths.ebook);
        a.audio_path = place(&audio_root, paths.audio);
        a.id = repo::insert(&self.db, &a, added_by_request_id).await?;
        tracing::info!(id = a.id, olid = %olid, name = %a.name, "author added");

        let monitor = WorkMonitor::Add {
            monitor: req.monitor,
            selected: &req.selected_work_ids,
        };
        if let Err(e) = self.sync_works(&a, &works, monitor, true).await {
            // Never leave a half-added author behind.
            let _ = repo::delete(&self.db, a.id).await;
            let _ = series_repo::prune(&self.db).await;
            return Err(e);
        }
        self.emit("author", "created", a.id);
        if scan {
            self.queue_scan(&[a.id], monitor_str(req.monitor)).await?;
        }
        if req.search_on_add && req.monitored {
            self.request_search(BookSearchRequest {
                kind: ContentSearchKind::Parent,
                ids: vec![a.id],
                format: None,
            })
            .await?;
        }
        self.author_row(a.id).await
    }

    pub async fn update_author(&self, id: i64, edit: AuthorEdit) -> Result<AuthorDto> {
        let mut a = self.author_row(id).await?;
        let old = a.clone();
        if let Some(m) = edit.monitored {
            a.monitored = m;
        }
        if let Some(m) = edit.monitor_new_items {
            a.monitor_new_items = m;
        }
        if let Some(t) = &edit.tags {
            a.tags = t.clone();
            a.tags.sort_unstable();
            a.tags.dedup();
        }
        let settings = self.settings().await?;
        let config = self.naming_config().await?;
        let folder = author_folder(&settings.naming, &a, &config);
        for format in BookFormat::ALL {
            let (profile_edit, root_edit, path_edit) = match format {
                BookFormat::Ebook => (edit.quality_profile_id, edit.root_folder_id, edit.path.as_deref()),
                BookFormat::Audiobook => (
                    edit.audio_quality_profile_id,
                    edit.audio_root_folder_id,
                    edit.audio_path.as_deref(),
                ),
            };
            let (profile, root, path) = match format {
                BookFormat::Ebook => (&mut a.quality_profile_id, &mut a.root_folder_id, &mut a.path),
                BookFormat::Audiobook => (
                    &mut a.audio_quality_profile_id,
                    &mut a.audio_root_folder_id,
                    &mut a.audio_path,
                ),
            };
            if let Some(p) = profile_edit {
                if let Some(p) = p {
                    self.check_profile(p, format).await?;
                }
                *profile = p;
            }
            if let Some(r) = root_edit {
                match r {
                    Some(r) => {
                        let rf = self.require_root(r, format).await?;
                        if *root != Some(rf.id) || path.is_none() {
                            let name = path
                                .as_deref()
                                .and_then(|p| Path::new(p).file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| folder.clone());
                            *path = Some(Path::new(&rf.path).join(name).to_string_lossy().into_owned());
                        }
                        *root = Some(rf.id);
                    }
                    None => *root = None,
                }
            }
            if let Some(p) = path_edit.map(str::trim).filter(|p| !p.is_empty()) {
                if !Path::new(p).is_absolute() {
                    return Err(BooksError::Validation("path must be absolute".into()));
                }
                *path = Some(content_roots::normalize(p));
            }
            if profile.is_some() && path.is_none() {
                let r = (*root).ok_or_else(|| {
                    BooksError::Validation(format!("a root folder is required for {}s", format.as_str()))
                })?;
                let rf = self.require_root(r, format).await?;
                *path = Some(Path::new(&rf.path).join(&folder).to_string_lossy().into_owned());
            }
        }
        if a.quality_profile_id.is_none() && a.audio_quality_profile_id.is_none() {
            return Err(BooksError::Validation(
                "an ebook or audiobook quality profile is required".into(),
            ));
        }
        if edit.move_files {
            for (from, to) in [(&old.path, &a.path), (&old.audio_path, &a.audio_path)] {
                if let (Some(from), Some(to)) = (from, to)
                    && from != to
                {
                    self.move_folder(Path::new(from), Path::new(to)).await?;
                }
            }
        }
        repo::save(&self.db, &a).await?;
        // A format the author no longer wants is unmonitored on every book.
        let ids: Vec<i64> = book_repo::load(
            &self.db,
            book_repo::BookFilter {
                ids: None,
                author_ids: Some(&[id]),
            },
        )
        .await?
        .iter()
        .map(|b| b.id)
        .collect();
        if a.quality_profile_id.is_none() && old.quality_profile_id.is_some() {
            book_repo::set_monitored(&self.db, &ids, Some(BookFormat::Ebook), false).await?;
        }
        if a.audio_quality_profile_id.is_none() && old.audio_quality_profile_id.is_some() {
            book_repo::set_monitored(&self.db, &ids, Some(BookFormat::Audiobook), false).await?;
        }
        self.emit("author", "updated", id);
        self.author_row(id).await
    }

    /// Moves an author folder to `to` (inside the book root folders).
    pub(crate) async fn move_folder(&self, from: &Path, to: &Path) -> Result<()> {
        if !from.exists() {
            return Ok(());
        }
        let guard = self.path_guard().await?;
        guard.check_existing(from)?;
        guard.check(to)?;
        if to.exists() {
            return Err(BooksError::Conflict(format!("{} already exists", to.display())));
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::rename(from, to).is_err() {
            // Cross-filesystem: move file by file, then remove the old tree.
            for f in reelix_import::files::scan(
                from,
                &reelix_import::files::ScanOptions {
                    include_non_video: true,
                    max_depth: 16,
                },
            )? {
                reelix_import::files::transfer(
                    &f.path,
                    &to.join(&f.relative),
                    reelix_import::files::TransferMode::Move,
                )?;
            }
            let _ = std::fs::remove_dir_all(from);
        }
        tracing::info!(from = %from.display(), to = %to.display(), "moved author folder");
        Ok(())
    }

    pub async fn delete_author(&self, id: i64, delete_files: bool) -> Result<()> {
        let a = self.author_row(id).await?;
        if delete_files {
            for p in [&a.path, &a.audio_path].into_iter().flatten() {
                self.remove_path(Path::new(p)).await?;
            }
        }
        repo::delete(&self.db, id).await?;
        series_repo::prune(&self.db).await?;
        self.emit("author", "deleted", id);
        tracing::info!(id, name = %a.name, delete_files, "author deleted");
        Ok(())
    }

    /// Open Library author search with in-library flags; `olid:OL…A` looks up one author.
    pub async fn lookup_authors(&self, term: &str) -> Result<Vec<AuthorLookupDto>> {
        let term = term.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let id = term
            .strip_prefix("olid:")
            .map(str::trim)
            .or_else(|| is_author_olid(term).then_some(term));
        let hits: Vec<AuthorSearchHit> = match id {
            Some(id) => {
                let m = self.metadata.author(id).await?;
                vec![AuthorSearchHit {
                    id: m.id,
                    name: m.name,
                    birth_date: m.birth_date,
                    top_work: None,
                    work_count: 0,
                    photo_url: m.photo_url,
                }]
            }
            None => self.metadata.search_authors(term).await?,
        };
        let index = repo::olid_index(&self.db).await?;
        Ok(hits
            .into_iter()
            .map(|h| AuthorLookupDto {
                author_id: index.get(&h.id).copied(),
                openlibrary_id: h.id,
                name: h.name,
                birth_date: h.birth_date,
                top_work: h.top_work,
                work_count: h.work_count,
                image: h.photo_url,
            })
            .collect())
    }
}
