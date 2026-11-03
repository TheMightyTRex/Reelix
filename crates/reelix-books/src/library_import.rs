//! Library import: existing author folders under a book root folder.

use reelix_api_types::books::{
    AddAuthorRequest, BookImportCandidateDto, BookLibraryImportRequest, NewBookMonitor,
};
use reelix_api_types::library::{ImportErrorDto, LibraryImportResult};
use reelix_core::BookFormat;
use reelix_library::content_roots;

use crate::BooksService;
use crate::authors::AuthorPaths;
use crate::error::{BooksError, Result};
use crate::repo::authors as author_repo;

impl BooksService {
    async fn book_root(&self, id: i64) -> Result<(content_roots::ContentRoot, BookFormat)> {
        let root = content_roots::get(&self.db, id)
            .await?
            .ok_or_else(|| BooksError::NotFound(format!("book root folder {id} not found")))?;
        let format = root
            .kind
            .book_format()
            .ok_or_else(|| BooksError::NotFound(format!("book root folder {id} not found")))?;
        Ok((root, format))
    }

    /// Sub-folders of a book root folder that are not an author folder yet, with Open Library
    /// author suggestions (folder name = author name).
    pub async fn import_scan(&self, root_folder_id: i64) -> Result<Vec<BookImportCandidateDto>> {
        let (root, _) = self.book_root(root_folder_id).await?;
        let dto = content_roots::dto(&root, &self.mapped_folders().await?).await;
        let mut out = Vec::new();
        for f in dto.unmapped_folders {
            let suggestions = match self.lookup_authors(&f.name).await {
                Ok(mut s) => {
                    s.truncate(5);
                    s
                }
                Err(e) => {
                    tracing::warn!(folder = %f.name, error = %e, "author lookup failed");
                    Vec::new()
                }
            };
            out.push(BookImportCandidateDto {
                guessed_author: Some(f.name.clone()).filter(|n| !n.trim().is_empty()),
                folder: f.name,
                path: f.path,
                kind: root.kind,
                suggestions,
            });
        }
        Ok(out)
    }

    /// Adds an author per item with the folder as its path for the root's format, scans it and
    /// applies the monitor option (so `existing`/`missing` see the files on disk).
    pub async fn library_import(&self, req: BookLibraryImportRequest) -> Result<LibraryImportResult> {
        let (root, format) = self.book_root(req.root_folder_id).await?;
        let mut result = LibraryImportResult::default();
        for item in req.items {
            let path = content_roots::normalize(&item.path);
            if let Err(e) = content_roots::ensure_inside(&path, [root.path.as_str()]) {
                result.errors.push(ImportErrorDto {
                    path: item.path,
                    message: e.to_string(),
                });
                continue;
            }
            let (profile, audio_profile) = match format {
                BookFormat::Ebook => (item.quality_profile_id, None),
                BookFormat::Audiobook => (None, item.audio_quality_profile_id.or(item.quality_profile_id)),
            };
            let add = AddAuthorRequest {
                openlibrary_id: item.openlibrary_id.clone(),
                quality_profile_id: profile,
                root_folder_id: (format == BookFormat::Ebook).then_some(root.id),
                audio_quality_profile_id: audio_profile,
                audio_root_folder_id: (format == BookFormat::Audiobook).then_some(root.id),
                monitored: true,
                monitor: item.monitor,
                monitor_new_items: NewBookMonitor::All,
                selected_work_ids: Vec::new(),
                tags: item.tags.clone(),
                search_on_add: false,
            };
            let paths = match format {
                BookFormat::Ebook => AuthorPaths {
                    ebook: Some(path.clone()),
                    audio: None,
                },
                BookFormat::Audiobook => AuthorPaths {
                    ebook: None,
                    audio: Some(path.clone()),
                },
            };
            // An author already in the library just gains this format's folder.
            if let Some(existing) = author_repo::id_by_olid(&self.db, item.openlibrary_id.trim()).await? {
                match self
                    .attach_folder(existing, format, &root, &path, profile.or(audio_profile))
                    .await
                {
                    Ok(()) => result.added.push(existing),
                    Err(e) => result.errors.push(ImportErrorDto {
                        path: item.path,
                        message: e.to_string(),
                    }),
                }
                continue;
            }
            match self.add_author_inner(add, paths, None, false).await {
                Ok(a) => {
                    if let Err(e) = self.scan_authors(&[a.id]).await {
                        tracing::warn!(author = a.id, error = %e, "scan after import failed");
                    }
                    let monitor = match item.monitor {
                        reelix_api_types::books::AuthorMonitor::Existing => Some("existing"),
                        reelix_api_types::books::AuthorMonitor::Missing => Some("missing"),
                        _ => None,
                    };
                    if let Some(m) = monitor {
                        self.apply_scan_monitor(a.id, m).await?;
                    }
                    result.added.push(a.id);
                }
                Err(e) => result.errors.push(ImportErrorDto {
                    path: item.path,
                    message: e.to_string(),
                }),
            }
        }
        Ok(result)
    }

    async fn attach_folder(
        &self,
        author_id: i64,
        format: BookFormat,
        root: &content_roots::ContentRoot,
        path: &str,
        profile: Option<i64>,
    ) -> Result<()> {
        let mut a = self.author_row(author_id).await?;
        let profile = match (format, profile) {
            (_, Some(p)) => p,
            (BookFormat::Ebook, None) => a
                .quality_profile_id
                .ok_or_else(|| BooksError::Validation("a quality profile is required for ebooks".into()))?,
            (BookFormat::Audiobook, None) => a.audio_quality_profile_id.ok_or_else(|| {
                BooksError::Validation("a quality profile is required for audiobooks".into())
            })?,
        };
        self.check_profile(profile, format).await?;
        match format {
            BookFormat::Ebook => {
                a.quality_profile_id = Some(profile);
                a.root_folder_id = Some(root.id);
                a.path = Some(path.to_string());
            }
            BookFormat::Audiobook => {
                a.audio_quality_profile_id = Some(profile);
                a.audio_root_folder_id = Some(root.id);
                a.audio_path = Some(path.to_string());
            }
        }
        author_repo::save(&self.db, &a).await?;
        self.scan_authors(&[author_id]).await?;
        self.emit("author", "updated", author_id);
        Ok(())
    }
}
