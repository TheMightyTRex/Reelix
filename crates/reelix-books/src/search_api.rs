//! What the pipeline needs to search for books: targets and release matching.

use std::collections::HashMap;

use chrono::NaiveDate;
use reelix_api_types::books::{AuthorDto, BookDto, BookFileDto};
use reelix_core::BookFormat;
use reelix_parser::{ParsedBook, QualityFamily};

use crate::BooksService;
use crate::error::{BooksError, Result};
use crate::repo::books::BookFilter;
use crate::repo::{authors as author_repo, clean, editions};
use crate::service::{file_for, monitored_for, profile_of};

/// Which targets a search wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    Missing,
    Cutoff,
    MissingOrCutoff,
    Any,
}

/// One book format to search for.
#[derive(Debug, Clone)]
pub struct BookTarget {
    pub author: AuthorDto,
    pub book: BookDto,
    pub format: BookFormat,
    pub quality_profile_id: i64,
    pub existing: Option<BookFileDto>,
    pub tags: Vec<i64>,
    /// Audiobook: known duration (minutes) for size checks.
    pub runtime_minutes: Option<u32>,
}

/// Borrowed view of [`BooksService`] for the pipeline.
pub struct BookSearchApi<'a> {
    pub(crate) svc: &'a BooksService,
}

fn make_target(a: &AuthorDto, b: &BookDto, format: BookFormat) -> Option<BookTarget> {
    let profile = profile_of(a, format)?;
    let existing = file_for(b, format).cloned();
    let runtime_minutes = b
        .audiobook_file
        .as_ref()
        .and_then(|f| f.duration_seconds)
        .filter(|_| format == BookFormat::Audiobook)
        .map(|s| s.div_ceil(60));
    Some(BookTarget {
        author: a.clone(),
        book: b.clone(),
        format,
        quality_profile_id: profile,
        existing,
        tags: a.tags.clone(),
        runtime_minutes,
    })
}

fn wanted_ok(t: &BookTarget, wanted: Wanted) -> bool {
    match (wanted, &t.existing) {
        (Wanted::Any, _) => true,
        (Wanted::Missing | Wanted::MissingOrCutoff, None) => true,
        (Wanted::Cutoff | Wanted::MissingOrCutoff, Some(f)) => !f.cutoff_met,
        _ => false,
    }
}

impl BookSearchApi<'_> {
    pub fn today(&self) -> NaiveDate {
        self.svc.today()
    }

    /// Fails with `Validation` when the author has no profile for `format`.
    pub async fn target(&self, book_id: i64, format: BookFormat) -> Result<BookTarget> {
        let b = self.svc.book_row(book_id).await?;
        let a = self.svc.author_row(b.author_id).await?;
        make_target(&a, &b, format).ok_or_else(|| {
            BooksError::Validation(format!("{} has no {} quality profile", a.name, format.as_str()))
        })
    }

    async fn collect(
        &self,
        f: BookFilter<'_>,
        format: Option<BookFormat>,
        wanted: Wanted,
    ) -> Result<Vec<BookTarget>> {
        let books = self.svc.load_books(f).await?;
        let authors: HashMap<i64, AuthorDto> = author_repo::load(&self.svc.db, None)
            .await?
            .into_iter()
            .map(|a| (a.id, a))
            .collect();
        let formats = format.map_or(BookFormat::ALL.to_vec(), |f| vec![f]);
        let mut out = Vec::new();
        for b in &books {
            let Some(a) = authors.get(&b.author_id).filter(|a| a.monitored) else {
                continue;
            };
            for f in &formats {
                if !monitored_for(b, *f) {
                    continue;
                }
                if let Some(t) = make_target(a, b, *f).filter(|t| wanted_ok(t, wanted)) {
                    out.push(t);
                }
            }
        }
        Ok(out)
    }

    /// Every monitored format target of the books (empty `book_ids` = whole library),
    /// filtered by `wanted`.
    pub async fn targets(
        &self,
        book_ids: &[i64],
        format: Option<BookFormat>,
        wanted: Wanted,
    ) -> Result<Vec<BookTarget>> {
        let f = BookFilter {
            ids: (!book_ids.is_empty()).then_some(book_ids),
            author_ids: None,
        };
        self.collect(f, format, wanted).await
    }

    pub async fn targets_for_authors(
        &self,
        author_ids: &[i64],
        format: Option<BookFormat>,
        wanted: Wanted,
    ) -> Result<Vec<BookTarget>> {
        if author_ids.is_empty() {
            return Ok(Vec::new());
        }
        let f = BookFilter {
            ids: None,
            author_ids: Some(author_ids),
        };
        self.collect(f, format, wanted).await
    }

    /// RSS / unknown-download mapping: ISBN/ASIN first, then author + title (clean-title
    /// compare). The format comes from the parse, else the book's single monitored format.
    pub async fn match_release(&self, parsed: &ParsedBook) -> Result<Option<(i64, BookFormat)>> {
        let books = self.svc.load_books(BookFilter::default()).await?;
        if books.is_empty() {
            return Ok(None);
        }
        let ids = editions::books_by_identifier(
            &self.svc.db,
            parsed.isbn.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            parsed.asin.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        )
        .await?;
        let mut book = books.iter().find(|b| ids.contains(&b.id));
        if book.is_none() {
            book = match_title(parsed, &books);
        }
        let Some(b) = book else { return Ok(None) };
        let from_quality = match parsed.quality.quality.family() {
            QualityFamily::Audiobook => Some(BookFormat::Audiobook),
            QualityFamily::Ebook if parsed.quality.quality != reelix_parser::Quality::UnknownText => {
                Some(BookFormat::Ebook)
            }
            _ => None,
        };
        let format =
            parsed
                .format
                .or(from_quality)
                .unwrap_or(if !b.monitored_ebook && b.monitored_audiobook {
                    BookFormat::Audiobook
                } else {
                    BookFormat::Ebook
                });
        Ok(Some((b.id, format)))
    }
}

/// Author + title match. With a parsed author both must match; without one the raw title
/// must contain both the author's name and the book title.
fn match_title<'b>(p: &ParsedBook, books: &'b [BookDto]) -> Option<&'b BookDto> {
    let title = clean(&p.title);
    let raw = clean(&p.raw_title);
    let author = p.author.as_deref().map(clean).filter(|a| !a.is_empty());
    let mut best: Option<(usize, &BookDto)> = None;
    for b in books {
        let bt = clean(&b.title);
        let ba = clean(&b.author_name);
        if bt.is_empty() {
            continue;
        }
        let author_ok = match &author {
            Some(a) => *a == ba || a.contains(&ba) || (ba.contains(a.as_str()) && a.len() >= 4),
            None => !ba.is_empty() && raw.contains(&ba),
        };
        if !author_ok {
            continue;
        }
        if author.is_some() && title == bt {
            return Some(b);
        }
        let hay = if author.is_some() { &title } else { &raw };
        if (bt.len() >= 4 || hay == &bt) && hay.contains(&bt) && best.is_none_or(|(l, _)| bt.len() > l) {
            best = Some((bt.len(), b));
        }
    }
    best.map(|(_, b)| b)
}
