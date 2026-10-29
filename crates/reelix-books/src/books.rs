//! Books: list/get/add/edit/monitor/delete, lookup, editions, series, files and wanted.

use std::path::Path;

use reelix_api_types::books::{
    AddAuthorRequest, AddBookRequest, AuthorMonitor, BookDto, BookEdit, BookFileDto, BookListQuery,
    BookLookupDto, BookMonitorRequest, BookSearchRequest, BookSeriesDto, BookWantedQuery, EditionDto,
    NewBookMonitor,
};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::library::Paged;
use reelix_core::BookFormat;
use reelix_metadata::{WorkMetadata, WorkSearchHit};

use crate::BooksService;
use crate::authors::{AuthorPaths, paginate};
use crate::error::{BooksError, Result};
use crate::metadata::{apply_editions, apply_work, blank_book};
use crate::repo::books::BookFilter;
use crate::repo::{books as repo, editions, files, series};
use crate::service::{file_for, monitored_for, path_of, profile_of};

pub(crate) fn is_work_olid(s: &str) -> bool {
    s.len() > 3
        && s.starts_with("OL")
        && s.ends_with('W')
        && s[2..s.len() - 1].chars().all(|c| c.is_ascii_digit())
}

fn isbn_of(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    (matches!(digits.len(), 10 | 13) && digits[..digits.len() - 1].chars().all(|c| c.is_ascii_digit()))
        .then_some(digits)
}

fn work_hit(w: &WorkMetadata) -> WorkSearchHit {
    WorkSearchHit {
        id: w.id.clone(),
        title: w.title.clone(),
        author_ids: w.authors.iter().map(|a| a.id.clone()).collect(),
        author_names: w.authors.iter().map(|a| a.name.clone()).collect(),
        first_publish_year: w.first_publish_year,
        cover_url: w.cover_url.clone(),
        isbns: Vec::new(),
        edition_count: 0,
    }
}

impl BooksService {
    pub async fn list_books(&self, q: &BookListQuery) -> Result<Paged<BookDto>> {
        let author_ids = q.author_id.map(|a| vec![a]);
        let mut items = self
            .load_books(BookFilter {
                ids: None,
                author_ids: author_ids.as_deref(),
            })
            .await?;
        let needle = q
            .search
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        items.retain(|b| {
            needle.as_ref().is_none_or(|n| {
                b.title.to_lowercase().contains(n)
                    || b.author_name.to_lowercase().contains(n)
                    || b.series.iter().any(|s| s.title.to_lowercase().contains(n))
            }) && q
                .monitored
                .is_none_or(|m| (b.monitored_ebook || b.monitored_audiobook) == m)
                && q.missing
                    .is_none_or(|f| monitored_for(b, f) && file_for(b, f).is_none())
        });
        Ok(paginate(items, q.page, q.page_size))
    }

    pub async fn get_book(&self, id: i64) -> Result<BookDto> {
        self.book_row(id).await
    }

    pub async fn book_by_work(&self, work_id: &str) -> Result<Option<BookDto>> {
        match repo::id_by_work(&self.db, work_id.trim()).await? {
            Some(id) => Ok(Some(self.book_row(id).await?)),
            None => Ok(None),
        }
    }

    /// Adds one work. When its first author is not in the library yet the author is added
    /// with only this book monitored.
    pub async fn add_book(&self, req: AddBookRequest) -> Result<BookDto> {
        let work_id = req.openlibrary_work_id.trim().to_string();
        if work_id.is_empty() {
            return Err(BooksError::Validation("openlibrary_work_id is required".into()));
        }
        if repo::id_by_work(&self.db, &work_id).await?.is_some() {
            return Err(BooksError::Conflict(format!(
                "book {work_id} is already in the library"
            )));
        }
        let work = self.metadata.work(&work_id).await?;
        let author_olid = work
            .authors
            .first()
            .map(|a| a.id.clone())
            .filter(|a| !a.is_empty())
            .ok_or_else(|| BooksError::Validation(format!("{work_id} has no author on Open Library")))?;
        let formats: Vec<BookFormat> = if req.formats.is_empty() {
            BookFormat::ALL.to_vec()
        } else {
            req.formats.clone()
        };
        let author = match self.author_by_openlibrary(&author_olid).await? {
            Some(a) => a,
            None => {
                let wants = |f: BookFormat| req.formats.is_empty() || req.formats.contains(&f);
                let add = AddAuthorRequest {
                    openlibrary_id: author_olid.clone(),
                    quality_profile_id: req.quality_profile_id.filter(|_| wants(BookFormat::Ebook)),
                    root_folder_id: req.root_folder_id,
                    audio_quality_profile_id: req
                        .audio_quality_profile_id
                        .filter(|_| wants(BookFormat::Audiobook)),
                    audio_root_folder_id: req.audio_root_folder_id,
                    monitored: true,
                    monitor: AuthorMonitor::None,
                    monitor_new_items: NewBookMonitor::None,
                    selected_work_ids: Vec::new(),
                    tags: req.tags.clone(),
                    search_on_add: false,
                };
                self.add_author_inner(add, AuthorPaths::default(), req.added_by_request_id, true)
                    .await?
            }
        };
        let id = match repo::id_by_work(&self.db, &work_id).await? {
            Some(id) => id,
            None => {
                let mut b = blank_book(&author);
                apply_work(&mut b, &work);
                let eds = self.metadata.work_editions(&work_id).await.unwrap_or_else(|e| {
                    tracing::warn!(work = %work_id, error = %e, "fetching editions failed");
                    Vec::new()
                });
                apply_editions(&mut b, &eds);
                b.id = repo::insert(&self.db, &b).await?;
                self.store_work_details(b.id, &work, &eds).await?;
                b.id
            }
        };
        let mut b = self.book_row(id).await?;
        b.monitored_ebook = formats.contains(&BookFormat::Ebook) && author.quality_profile_id.is_some();
        b.monitored_audiobook =
            formats.contains(&BookFormat::Audiobook) && author.audio_quality_profile_id.is_some();
        if req.added_by_request_id.is_some() {
            b.added_by_request_id = req.added_by_request_id;
        }
        repo::save(&self.db, &b).await?;
        self.emit("book", "created", id);
        tracing::info!(id, work = %work_id, title = %b.title, "book added");
        if req.search_on_add && author.monitored && (b.monitored_ebook || b.monitored_audiobook) {
            self.request_search(BookSearchRequest {
                kind: ContentSearchKind::Item,
                ids: vec![id],
                format: None,
            })
            .await?;
        }
        self.book_row(id).await
    }

    pub async fn update_book(&self, id: i64, edit: BookEdit) -> Result<BookDto> {
        let b = self.book_row(id).await?;
        if let Some(m) = edit.monitored_ebook {
            repo::set_monitored(&self.db, &[id], Some(BookFormat::Ebook), m).await?;
        }
        if let Some(m) = edit.monitored_audiobook {
            repo::set_monitored(&self.db, &[id], Some(BookFormat::Audiobook), m).await?;
        }
        if let Some(e) = edit.preferred_edition_id
            && !editions::set_preferred(&self.db, b.id, e).await?
        {
            return Err(BooksError::Validation(format!(
                "edition {e} is not an edition of book {id}"
            )));
        }
        self.emit("book", "updated", id);
        self.book_row(id).await
    }

    pub async fn monitor_books(&self, req: BookMonitorRequest) -> Result<Vec<BookDto>> {
        if req.book_ids.is_empty() {
            return Ok(Vec::new());
        }
        repo::set_monitored(&self.db, &req.book_ids, req.format, req.monitored).await?;
        for id in &req.book_ids {
            self.emit("book", "updated", *id);
        }
        self.load_books(BookFilter {
            ids: Some(&req.book_ids),
            author_ids: None,
        })
        .await
    }

    pub async fn delete_book(&self, id: i64, delete_files: bool) -> Result<()> {
        let b = self.book_row(id).await?;
        if delete_files {
            for f in files::list(&self.db, Some(&[id])).await? {
                self.remove_book_file(&f).await?;
            }
        }
        repo::delete(&self.db, id).await?;
        series::prune(&self.db).await?;
        self.emit("book", "deleted", id);
        self.emit("author", "updated", b.author_id);
        tracing::info!(id, title = %b.title, delete_files, "book deleted");
        Ok(())
    }

    /// Open Library work search with in-library flags; `isbn:…`, `olid:OL…W` and bare
    /// ISBNs look up one work, `asin:…` searches by ASIN.
    pub async fn lookup_books(&self, term: &str) -> Result<Vec<BookLookupDto>> {
        let term = term.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let lower = term.to_ascii_lowercase();
        let isbn = lower
            .strip_prefix("isbn:")
            .map(str::trim)
            .and_then(isbn_of)
            .or_else(|| isbn_of(term).filter(|_| term.chars().all(|c| c.is_ascii_digit() || c == '-')));
        let olid = term
            .strip_prefix("olid:")
            .map(str::trim)
            .or_else(|| is_work_olid(term).then_some(term));
        let (hits, isbn13): (Vec<WorkSearchHit>, Option<String>) = if let Some(isbn) = isbn {
            let (w, e) = self.metadata.by_isbn(&isbn).await?;
            (
                vec![work_hit(&w)],
                e.isbn13.or(Some(isbn).filter(|i| i.len() == 13)),
            )
        } else if let Some(id) = olid {
            (vec![work_hit(&self.metadata.work(id).await?)], None)
        } else if let Some(asin) = lower.strip_prefix("asin:") {
            (self.metadata.search_works(asin.trim()).await?, None)
        } else {
            (self.metadata.search_works(term).await?, None)
        };
        let index = repo::work_index(&self.db).await?;
        Ok(hits
            .into_iter()
            .map(|h| BookLookupDto {
                book_id: index.get(&h.id).copied(),
                isbn13: isbn13
                    .clone()
                    .or_else(|| h.isbns.iter().find(|i| i.len() == 13).cloned()),
                openlibrary_work_id: h.id,
                title: h.title,
                author_name: h.author_names.first().cloned(),
                author_openlibrary_id: h.author_ids.first().cloned(),
                year: h.first_publish_year,
                cover: h.cover_url,
            })
            .collect())
    }

    /// Editions of a book (fetched from Open Library the first time).
    pub async fn editions(&self, book_id: i64) -> Result<Vec<EditionDto>> {
        self.book_row(book_id).await?;
        if editions::count(&self.db, book_id).await? == 0 {
            self.refresh_editions(book_id).await?;
        }
        editions::list(&self.db, book_id).await
    }

    pub async fn book_series(&self) -> Result<Vec<BookSeriesDto>> {
        series::list(&self.db).await
    }

    pub async fn book_files(&self, book_id: i64) -> Result<Vec<BookFileDto>> {
        let b = self.book_row(book_id).await?;
        Ok([b.ebook_file, b.audiobook_file].into_iter().flatten().collect())
    }

    /// Deletes a book file (every part) from disk (recycle bin when configured) and the database.
    pub async fn delete_book_file(&self, file_id: i64) -> Result<()> {
        let f = files::get(&self.db, file_id)
            .await?
            .ok_or_else(|| BooksError::NotFound(format!("book file {file_id} not found")))?;
        self.remove_book_file(&f).await?;
        files::delete(&self.db, file_id).await?;
        self.emit("book_file", "deleted", file_id);
        self.emit("book", "updated", f.book_id);
        Ok(())
    }

    /// Removes the files of a record from disk; the folder of a multi-file audiobook goes
    /// too once empty.
    pub(crate) async fn remove_book_file(&self, f: &files::FileRow) -> Result<Vec<std::path::PathBuf>> {
        let b = self.book_row(f.book_id).await?;
        let a = self.author_row(b.author_id).await?;
        let Some(base) = path_of(&a, f.format) else {
            return Ok(Vec::new());
        };
        let base = Path::new(base);
        let mut removed = Vec::new();
        for rel in f.file_paths() {
            let p = base.join(&rel);
            if p.exists() {
                self.remove_path(&p).await?;
                removed.push(p);
            }
        }
        if !f.parts.is_empty() {
            let folder = base.join(&f.relative_path);
            if folder.is_dir() && std::fs::read_dir(&folder).is_ok_and(|mut d| d.next().is_none()) {
                let _ = std::fs::remove_dir(&folder);
            }
        }
        Ok(removed)
    }

    /// Monitored books missing a wanted format (`kind=missing`, released only) or whose file
    /// is below the profile cutoff (`kind=cutoff`), newest first.
    pub async fn wanted(&self, q: &BookWantedQuery) -> Result<Paged<BookDto>> {
        let cutoff = q
            .kind
            .as_deref()
            .is_some_and(|k| k.eq_ignore_ascii_case("cutoff"));
        let today = self.today();
        let authors: std::collections::HashMap<i64, _> = crate::repo::authors::load(&self.db, None)
            .await?
            .into_iter()
            .map(|a| (a.id, a))
            .collect();
        let mut items = self.load_books(BookFilter::default()).await?;
        items.retain(|b| {
            let Some(a) = authors.get(&b.author_id).filter(|a| a.monitored) else {
                return false;
            };
            let formats: Vec<BookFormat> = q.format.map_or(BookFormat::ALL.to_vec(), |f| vec![f]);
            formats.iter().any(|f| {
                if profile_of(a, *f).is_none() || !monitored_for(b, *f) {
                    return false;
                }
                match file_for(b, *f) {
                    None if !cutoff => !crate::metadata::is_future(b.release_date, b.year, today),
                    Some(file) if cutoff => !file.cutoff_met,
                    _ => false,
                }
            })
        });
        items.sort_by(|a, b| {
            b.release_date
                .cmp(&a.release_date)
                .then(a.sort_title.cmp(&b.sort_title))
        });
        Ok(paginate(items, q.page, q.page_size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers() {
        assert_eq!(isbn_of("978-0-7653-1178-8").as_deref(), Some("9780765311788"));
        assert_eq!(isbn_of("076531178X").as_deref(), Some("076531178X"));
        assert_eq!(isbn_of("dune"), None);
        assert!(is_work_olid("OL45883W"));
        assert!(!is_work_olid("OL45883A"));
    }
}
