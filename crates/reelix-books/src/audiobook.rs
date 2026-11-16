//! Audiobook details (RFX-86): narrators, length, abridgement and recording date of a book from
//! Audible, Audnexus and (when activated) Libro.fm.
//!
//! The chosen recording of each book is kept in `metadata_cache` (provider `book_audiobook`,
//! key the book id), so it survives restarts and a failed lookup never loses it. A found
//! recording is looked up again after 30 days, a book no source knew after 7 days. Only books
//! monitored as audiobooks are looked up during a refresh.

use chrono::{DateTime, Duration, Utc};
use reelix_api_types::books::{AudiobookDetailsDto, AudiobookMetadataSettingsDto, BookDto, BookSeriesRefDto};
use reelix_metadata::{AudiobookDetails, AudiobookQuery, AudiobookSources, AudiobookSourcesOptions};

use crate::BooksService;
use crate::error::{BooksError, Result};
use crate::repo::books as book_repo;

const PROVIDER: &str = "book_audiobook";
/// A found recording is looked up again after this many days.
pub(crate) const FOUND_DAYS: i64 = 30;
/// A book no source knew is asked about again after this many days.
pub(crate) const MISSING_DAYS: i64 = 7;
/// Most books looked up in one refresh run, so a large library never floods the sources; the
/// rest follow in later daily runs.
pub(crate) const MAX_PER_REFRESH: usize = 200;
/// Rows stay in `metadata_cache` this long; staleness is decided from `looked_up`.
const KEEP_SECS: i64 = 400 * 24 * 3600;

impl BooksService {
    /// Points the audiobook sources at other addresses (tests). Later calls are ignored.
    #[doc(hidden)]
    pub fn set_audiobook_sources_options(&self, opts: AudiobookSourcesOptions) {
        let _ = self.audiobook_template.set(opts);
    }

    /// The sources for the current settings, reused while the settings stay the same so the
    /// pacing and memory cache carry over between books.
    fn audiobook_sources(&self, s: &AudiobookMetadataSettingsDto) -> AudiobookSources {
        let mut held = self.audiobook_sources.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((settings, sources)) = held.as_ref()
            && settings == s
        {
            return sources.clone();
        }
        let mut opts = self.audiobook_template.get().cloned().unwrap_or_default();
        opts.audible = s.audible;
        opts.audnexus = s.audnexus;
        opts.librofm = s.librofm;
        opts.region.clone_from(&s.region);
        let sources = AudiobookSources::new(self.http.clone(), opts);
        *held = Some((s.clone(), sources.clone()));
        sources
    }

    /// The stored recording of a book, if one was looked up.
    pub async fn stored_audiobook(&self, book_id: i64) -> Result<Option<AudiobookDetailsDto>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT payload FROM metadata_cache WHERE provider = ? AND key = ?")
                .bind(PROVIDER)
                .bind(book_id.to_string())
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.and_then(|(p,)| serde_json::from_str(&p).ok()))
    }

    async fn store_audiobook(&self, d: &AudiobookDetailsDto) -> Result<()> {
        let payload = serde_json::to_string(d).map_err(|e| BooksError::Internal(e.to_string()))?;
        sqlx::query(
            "INSERT INTO metadata_cache (provider, key, payload, expires_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (provider, key) DO UPDATE SET payload = excluded.payload, expires_at = excluded.expires_at",
        )
        .bind(PROVIDER)
        .bind(d.book_id.to_string())
        .bind(payload)
        .bind(Utc::now().timestamp() + KEEP_SECS)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// The recording of a book: the stored one while it is fresh, otherwise looked up again
    /// (always when `force`). When every source fails, the stored details are kept and returned;
    /// with none stored, the error is returned.
    pub async fn audiobook_details(&self, book_id: i64, force: bool) -> Result<AudiobookDetailsDto> {
        let book = self.get_book(book_id).await?;
        let stored = self.stored_audiobook(book_id).await?;
        if !force
            && let Some(s) = &stored
            && is_fresh(s, Utc::now())
        {
            return Ok(s.clone());
        }
        let settings = self.settings().await?.audiobook_metadata;
        if !(settings.audible || settings.audnexus || settings.librofm) {
            return Err(BooksError::Validation(
                "no audiobook source is activated in the book settings".into(),
            ));
        }
        let sources = self.audiobook_sources(&settings);
        let found = match sources.lookup(&query_for(&book)).await {
            Ok(found) => found,
            Err(e) => {
                return match stored {
                    Some(s) => {
                        tracing::warn!(book_id, error = %e, "audiobook lookup failed; keeping stored details");
                        Ok(s)
                    }
                    None => Err(e.into()),
                };
            }
        };
        let dto = to_dto(book_id, found.as_ref(), Utc::now());
        self.store_audiobook(&dto).await?;
        if let Some(asin) = dto.asin.clone()
            && book.asin.is_none()
        {
            let mut b = book;
            b.asin = Some(asin);
            book_repo::save(&self.db, &b).await?;
            self.emit("book", "updated", b.id);
        }
        Ok(dto)
    }

    /// Runs [`Self::refresh_audiobooks`] for the books of the given authors (all when empty).
    pub async fn refresh_audiobooks_of(&self, author_ids: &[i64]) -> Result<usize> {
        let books = book_repo::load(
            &self.db,
            book_repo::BookFilter {
                ids: None,
                author_ids: (!author_ids.is_empty()).then_some(author_ids),
            },
        )
        .await?;
        Ok(self.refresh_audiobooks(&books).await)
    }

    /// Looks up books monitored as audiobooks whose details are missing or stale (at most
    /// [`MAX_PER_REFRESH`]). Failures are logged and skipped. Returns how many were looked up.
    pub(crate) async fn refresh_audiobooks(&self, books: &[BookDto]) -> usize {
        let Ok(settings) = self.settings().await else {
            return 0;
        };
        let s = settings.audiobook_metadata;
        if !(s.audible || s.audnexus || s.librofm) {
            return 0;
        }
        let now = Utc::now();
        let mut done = 0;
        for b in books.iter().filter(|b| b.monitored_audiobook && b.id > 0) {
            if done >= MAX_PER_REFRESH {
                break;
            }
            let fresh = matches!(self.stored_audiobook(b.id).await, Ok(Some(d)) if is_fresh(&d, now));
            if fresh {
                continue;
            }
            done += 1;
            if let Err(e) = self.audiobook_details(b.id, true).await {
                tracing::debug!(book_id = b.id, error = %e, "audiobook details not refreshed");
            }
        }
        done
    }

    /// The narrators of the stored recording, joined with commas (naming and tags fallback).
    pub(crate) async fn stored_narrators(&self, book_id: i64) -> Option<String> {
        let d = self.stored_audiobook(book_id).await.ok().flatten()?;
        (!d.narrators.is_empty()).then(|| d.narrators.join(", "))
    }
}

fn query_for(b: &BookDto) -> AudiobookQuery {
    let mut authors: Vec<String> = b
        .credits
        .iter()
        .filter(|c| c.role.as_deref().is_none_or(|r| r.eq_ignore_ascii_case("author")))
        .map(|c| c.name.clone())
        .collect();
    if authors.is_empty() && !b.author_name.trim().is_empty() {
        authors.push(b.author_name.clone());
    }
    AudiobookQuery {
        title: b.title.clone(),
        authors,
        asin: b.asin.clone(),
        isbn: None,
    }
}

pub(crate) fn is_fresh(d: &AudiobookDetailsDto, now: DateTime<Utc>) -> bool {
    let days = if d.found { FOUND_DAYS } else { MISSING_DAYS };
    now - d.looked_up < Duration::days(days)
}

pub(crate) fn to_dto(book_id: i64, d: Option<&AudiobookDetails>, now: DateTime<Utc>) -> AudiobookDetailsDto {
    let Some(d) = d else {
        return AudiobookDetailsDto {
            book_id,
            found: false,
            asin: None,
            isbn: None,
            title: None,
            authors: Vec::new(),
            narrators: Vec::new(),
            runtime_minutes: None,
            abridged: None,
            recording_date: None,
            language: None,
            publisher: None,
            series: Vec::new(),
            cover_url: None,
            region: None,
            sources: Vec::new(),
            url: None,
            looked_up: now,
        };
    };
    AudiobookDetailsDto {
        book_id,
        found: true,
        asin: d.asin.clone(),
        isbn: d.isbn.clone(),
        title: Some(d.title.clone()),
        authors: d.authors.clone(),
        narrators: d.narrators.clone(),
        runtime_minutes: d.runtime_minutes,
        abridged: d.abridged,
        recording_date: d.release_date.clone(),
        language: d.language.clone(),
        publisher: d.publisher.clone(),
        series: d
            .series
            .iter()
            .map(|s| BookSeriesRefDto {
                id: 0,
                title: s.title.clone(),
                position: s.position.clone(),
            })
            .collect(),
        cover_url: d.cover_url.clone(),
        region: d.region.clone(),
        sources: d.sources.clone(),
        url: d.url.clone(),
        looked_up: now,
    }
}

/// Validates the audiobook source settings.
pub(crate) fn settings_errors(s: &AudiobookMetadataSettingsDto) -> Option<String> {
    reelix_metadata::audiobooks::region_tld(&s.region)
        .is_none()
        .then(|| {
            let codes: Vec<&str> = reelix_metadata::audiobooks::REGIONS
                .iter()
                .map(|(c, _)| *c)
                .collect();
            format!("audiobook_metadata.region: must be one of {}", codes.join(", "))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness() {
        let now = Utc::now();
        let mut d = to_dto(1, None, now - Duration::days(8));
        assert!(!is_fresh(&d, now));
        d.looked_up = now - Duration::days(6);
        assert!(is_fresh(&d, now));
        let found = AudiobookDetails {
            title: "Dune".into(),
            ..AudiobookDetails::default()
        };
        let d = to_dto(1, Some(&found), now - Duration::days(20));
        assert!(d.found);
        assert!(is_fresh(&d, now));
    }

    #[test]
    fn region_is_checked() {
        let mut s = AudiobookMetadataSettingsDto::default();
        assert!(settings_errors(&s).is_none());
        s.region = "xx".into();
        assert!(settings_errors(&s).unwrap().contains("uk"));
    }
}
