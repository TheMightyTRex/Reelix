//! Metadata refresh: author details, new works, editions and series.

use std::collections::HashMap;

use chrono::Utc;
use reelix_api_types::books::{AuthorDto, AuthorMonitor, BookDto, NewBookMonitor};
use reelix_core::BookFormat;
use reelix_metadata::{EditionMetadata, WorkMetadata};

use crate::BooksService;
use crate::error::Result;
use crate::metadata::{apply_author, apply_editions, apply_work, blank_book, is_future, series_links};
use crate::repo::books::BookFilter;
use crate::repo::{authors as author_repo, books as repo, editions, series};

/// How works found for an author get monitored.
#[derive(Debug, Clone, Copy)]
pub(crate) enum WorkMonitor<'a> {
    /// Adding the author.
    Add {
        monitor: AuthorMonitor,
        selected: &'a [String],
    },
    /// Refresh: new works follow `monitor_new_items`.
    NewItems,
}

impl BooksService {
    /// Inserts new works (monitored per `mode`) and refreshes the metadata of known ones.
    /// Editions are fetched for monitored books when `editions` is true (or when none are
    /// stored yet). Returns the ids of the inserted books.
    pub(crate) async fn sync_works(
        &self,
        a: &AuthorDto,
        works: &[WorkMetadata],
        mode: WorkMonitor<'_>,
        fetch_editions: bool,
    ) -> Result<Vec<i64>> {
        let today = self.today();
        let index = repo::work_index(&self.db).await?;
        let existing: HashMap<i64, BookDto> = repo::load(
            &self.db,
            BookFilter {
                ids: None,
                author_ids: Some(&[a.id]),
            },
        )
        .await?
        .into_iter()
        .map(|b| (b.id, b))
        .collect();
        let mut inserted = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for w in works {
            if w.id.trim().is_empty() || w.title.trim().is_empty() || !seen.insert(w.id.clone()) {
                continue;
            }
            let (mut b, is_new) = match index.get(&w.id) {
                // Known work of another author: left alone.
                Some(id) if !existing.contains_key(id) => continue,
                Some(id) => (existing[id].clone(), false),
                None => (blank_book(a), true),
            };
            apply_work(&mut b, w);
            if is_new {
                let monitored = match mode {
                    WorkMonitor::Add { monitor, selected } => match monitor {
                        AuthorMonitor::All | AuthorMonitor::Missing => true,
                        AuthorMonitor::Future => is_future(b.release_date, b.year, today),
                        AuthorMonitor::Existing | AuthorMonitor::None => false,
                        AuthorMonitor::Selected => selected.iter().any(|s| s.trim() == w.id),
                    },
                    WorkMonitor::NewItems => a.monitor_new_items == NewBookMonitor::All,
                };
                b.monitored_ebook = monitored && a.quality_profile_id.is_some();
                b.monitored_audiobook = monitored && a.audio_quality_profile_id.is_some();
            }
            let wants_editions = (b.monitored_ebook || b.monitored_audiobook)
                && (fetch_editions || is_new || b.id == 0 || editions::count(&self.db, b.id).await? == 0);
            let eds = if wants_editions {
                match self.metadata.work_editions(&w.id).await {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(work = %w.id, error = %e, "fetching editions failed");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            apply_editions(&mut b, &eds);
            if is_new {
                b.id = repo::insert(&self.db, &b).await?;
                inserted.push(b.id);
            } else {
                repo::save(&self.db, &b).await?;
            }
            self.store_work_details(b.id, w, &eds).await?;
        }
        Ok(inserted)
    }

    /// Credits, series and editions of a stored book.
    pub(crate) async fn store_work_details(
        &self,
        book_id: i64,
        w: &WorkMetadata,
        eds: &[EditionMetadata],
    ) -> Result<()> {
        let mut credits = w.authors.clone();
        for c in &mut credits {
            if c.role.is_none() {
                c.role = Some("author".into());
            }
        }
        repo::set_credits(&self.db, book_id, &credits).await?;
        let links = series_links(w, eds);
        if !links.is_empty() || !w.series.is_empty() {
            series::set_links(&self.db, book_id, &links).await?;
        }
        for e in eds {
            if !e.id.trim().is_empty() {
                editions::upsert(&self.db, book_id, e).await?;
            }
        }
        Ok(())
    }

    /// Refreshes the given authors (all when `None`); returns how many failed. Scheduled
    /// (non-manual) refreshes skip authors refreshed in the last 12 hours.
    pub async fn refresh_authors(&self, ids: Option<Vec<i64>>, manual: bool) -> Result<usize> {
        let ids = ids.filter(|v| !v.is_empty());
        let authors = author_repo::load(&self.db, ids.as_deref()).await?;
        let cutoff = Utc::now() - chrono::Duration::hours(12);
        let mut failed = 0;
        for mut a in authors {
            // Custom authors keep their typed details; their books come from the scan.
            if a.custom || (!manual && a.last_refreshed.is_some_and(|t| t > cutoff)) {
                continue;
            }
            match self.refresh_author(&mut a, manual).await {
                Ok(added) => {
                    tracing::info!(id = a.id, name = %a.name, added, "author refreshed");
                    self.emit("author", "updated", a.id);
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(id = a.id, name = %a.name, error = %e, "author refresh failed");
                }
            }
        }
        Ok(failed)
    }

    async fn refresh_author(&self, a: &mut AuthorDto, manual: bool) -> Result<usize> {
        let meta = self.metadata.author(&a.openlibrary_id).await?;
        let works = self.metadata.author_works(&a.openlibrary_id).await?;
        apply_author(a, &meta);
        author_repo::save(&self.db, a).await?;
        let added = self.sync_works(a, &works, WorkMonitor::NewItems, manual).await?;
        for id in &added {
            self.emit("book", "created", *id);
        }
        Ok(added.len())
    }

    /// Fetches and stores the editions of one book (also refreshes identifiers and series).
    pub(crate) async fn refresh_editions(&self, book_id: i64) -> Result<()> {
        let mut b = self.book_row(book_id).await?;
        if b.openlibrary_work_id.starts_with("custom:") {
            return Ok(()); // a book of a custom author: no Open Library work
        }
        let eds = self.metadata.work_editions(&b.openlibrary_work_id).await?;
        let w = WorkMetadata {
            id: b.openlibrary_work_id.clone(),
            title: b.title.clone(),
            authors: b
                .credits
                .iter()
                .map(|c| reelix_metadata::WorkAuthor {
                    id: c.openlibrary_id.clone(),
                    name: c.name.clone(),
                    role: c.role.clone(),
                })
                .collect(),
            series: b
                .series
                .iter()
                .map(|s| reelix_metadata::SeriesRef {
                    title: s.title.clone(),
                    position: s.position.clone(),
                })
                .collect(),
            ..Default::default()
        };
        apply_editions(&mut b, &eds);
        repo::save(&self.db, &b).await?;
        self.store_work_details(book_id, &w, &eds).await
    }

    /// Unmonitors formats per a post-scan [`AuthorMonitor`]: `existing` keeps only formats with
    /// files, `missing` only formats without.
    pub(crate) async fn apply_scan_monitor(&self, author_id: i64, monitor: &str) -> Result<()> {
        let a = self.author_row(author_id).await?;
        let books = self
            .load_books(BookFilter {
                ids: None,
                author_ids: Some(&[author_id]),
            })
            .await?;
        for mut b in books {
            for format in BookFormat::ALL {
                let wanted = crate::service::profile_of(&a, format).is_some();
                let has = crate::service::file_for(&b, format).is_some();
                let m = wanted
                    && match monitor {
                        "existing" => has,
                        "missing" => !has,
                        _ => crate::service::monitored_for(&b, format),
                    };
                match format {
                    BookFormat::Ebook => b.monitored_ebook = m,
                    BookFormat::Audiobook => b.monitored_audiobook = m,
                }
            }
            repo::set_monitored(&self.db, &[b.id], Some(BookFormat::Ebook), b.monitored_ebook).await?;
            repo::set_monitored(
                &self.db,
                &[b.id],
                Some(BookFormat::Audiobook),
                b.monitored_audiobook,
            )
            .await?;
        }
        self.emit("author", "updated", author_id);
        Ok(())
    }
}
