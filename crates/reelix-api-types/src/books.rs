//! Books (M8): authors, books (ebook + audiobook formats), editions, series, files, lookup,
//! library import, renaming and book settings.
//!
//! All routes are under `/api/v1` and need `MANAGE_LIBRARY`.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::content::{BookFormat, ContentKind, ContentSearchKind};
use crate::library::{ImagesDto, RatingsDto, SortDir};

pub use crate::library::{LibraryImportResult, Paged, RenamePreviewDto};
pub use reelix_parser::QualityModel;

fn yes() -> bool {
    true
}

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
fn double<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// Which new works found on refresh get monitored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NewBookMonitor {
    #[default]
    All,
    None,
}

/// Which books get monitored when an author is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AuthorMonitor {
    #[default]
    All,
    Future,
    Missing,
    Existing,
    None,
    /// Only `selected_work_ids`.
    Selected,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuthorStatisticsDto {
    pub book_count: u32,
    pub ebook_file_count: u32,
    pub audiobook_file_count: u32,
    pub monitored_count: u32,
    pub size_on_disk: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuthorDto {
    pub id: i64,
    pub openlibrary_id: String,
    pub name: String,
    pub sort_name: String,
    pub overview: Option<String>,
    pub birth_date: Option<String>,
    pub death_date: Option<String>,
    pub images: ImagesDto,
    pub links: Vec<String>,
    pub monitored: bool,
    pub monitor_new_items: NewBookMonitor,
    /// Ebook format; `None` = ebooks not wanted.
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub path: Option<String>,
    /// Audiobook format; `None` = audiobooks not wanted.
    pub audio_quality_profile_id: Option<i64>,
    pub audio_root_folder_id: Option<i64>,
    pub audio_path: Option<String>,
    pub tags: Vec<i64>,
    pub statistics: AuthorStatisticsDto,
    pub added: DateTime<Utc>,
    pub last_refreshed: Option<DateTime<Utc>>,
    /// Added with the owner's own details (`POST /customitems`): no outside match, and metadata
    /// refreshes keep the typed details.
    #[serde(default)]
    pub custom: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookSeriesRefDto {
    pub id: i64,
    pub title: String,
    pub position: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookCreditDto {
    pub openlibrary_id: String,
    pub name: String,
    pub role: Option<String>,
    /// Library author id when the author is in the library.
    pub author_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookFilePartDto {
    pub relative_path: String,
    pub size: u64,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookFileDto {
    pub id: i64,
    pub book_id: i64,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub format: BookFormat,
    /// Relative to the author's path / audio path (a folder for multi-file audiobooks).
    pub relative_path: String,
    pub path: String,
    pub parts: Vec<BookFilePartDto>,
    pub size: u64,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub quality: QualityModel,
    pub quality_name: String,
    pub bitrate_kbps: Option<u32>,
    pub duration_seconds: Option<u32>,
    pub narrator: Option<String>,
    pub edition_id: Option<i64>,
    pub release_group: Option<String>,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    pub custom_format_ids: Vec<i64>,
    pub custom_formats: Vec<String>,
    pub custom_format_score: i32,
    pub cutoff_met: bool,
    pub date_added: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookDto {
    pub id: i64,
    pub author_id: i64,
    pub author_name: String,
    pub openlibrary_work_id: String,
    pub title: String,
    pub sort_title: String,
    pub subtitle: Option<String>,
    pub overview: Option<String>,
    pub release_date: Option<NaiveDate>,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub images: ImagesDto,
    pub ratings: RatingsDto,
    pub isbn13: Option<String>,
    pub asin: Option<String>,
    pub page_count: Option<u32>,
    pub language: Option<String>,
    pub series: Vec<BookSeriesRefDto>,
    pub credits: Vec<BookCreditDto>,
    pub monitored_ebook: bool,
    pub monitored_audiobook: bool,
    pub ebook_file: Option<BookFileDto>,
    pub audiobook_file: Option<BookFileDto>,
    pub added_by_request_id: Option<i64>,
    pub added: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EditionDto {
    pub id: i64,
    pub book_id: i64,
    pub openlibrary_edition_id: String,
    pub title: String,
    pub isbn13: Option<String>,
    pub isbn10: Option<String>,
    pub asin: Option<String>,
    pub publisher: Option<String>,
    pub publish_date: Option<String>,
    pub language: Option<String>,
    pub page_count: Option<u32>,
    pub physical_format: Option<String>,
    pub is_audiobook: bool,
    pub images: ImagesDto,
    /// Preferred edition for metadata / tag writing.
    pub monitored: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookSeriesDto {
    pub id: i64,
    pub title: String,
    /// `(position, book id)` pairs.
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Object>))]
    pub books: Vec<(String, i64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuthorLookupDto {
    pub openlibrary_id: String,
    pub name: String,
    pub birth_date: Option<String>,
    pub top_work: Option<String>,
    pub work_count: u32,
    pub image: Option<String>,
    /// Library author id when already added.
    pub author_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookLookupDto {
    pub openlibrary_work_id: String,
    pub title: String,
    pub author_name: Option<String>,
    pub author_openlibrary_id: Option<String>,
    pub year: Option<i32>,
    pub cover: Option<String>,
    pub isbn13: Option<String>,
    /// Library book id when already added.
    pub book_id: Option<i64>,
}

/// `GET /authors/lookup?term=` / `GET /books/lookup?term=`; `isbn:…`, `asin:…`, `olid:…`
/// prefixes are supported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct LookupTermQuery {
    pub term: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AddAuthorRequest {
    pub openlibrary_id: String,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub audio_quality_profile_id: Option<i64>,
    pub audio_root_folder_id: Option<i64>,
    #[serde(default = "yes")]
    pub monitored: bool,
    #[serde(default)]
    pub monitor: AuthorMonitor,
    #[serde(default)]
    pub monitor_new_items: NewBookMonitor,
    #[serde(default)]
    pub selected_work_ids: Vec<String>,
    #[serde(default)]
    pub tags: Vec<i64>,
    #[serde(default)]
    pub search_on_add: bool,
}

/// Adds one book; creates the author (monitor None) when needed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AddBookRequest {
    pub openlibrary_work_id: String,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub formats: Vec<BookFormat>,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub audio_quality_profile_id: Option<i64>,
    pub audio_root_folder_id: Option<i64>,
    #[serde(default)]
    pub tags: Vec<i64>,
    #[serde(default)]
    pub search_on_add: bool,
    #[serde(default)]
    pub added_by_request_id: Option<i64>,
}

/// `PUT /authors/{id}`; every field is optional (absent = unchanged). For the nullable
/// profile/root fields an explicit `null` clears the value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuthorEdit {
    #[serde(default)]
    pub monitored: Option<bool>,
    #[serde(default)]
    pub monitor_new_items: Option<NewBookMonitor>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<i64>))]
    pub quality_profile_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<i64>))]
    pub root_folder_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<i64>))]
    pub audio_quality_profile_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<i64>))]
    pub audio_root_folder_id: Option<Option<i64>>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub audio_path: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
    /// Move existing files when `path`/`audio_path` change.
    #[serde(default)]
    pub move_files: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookEdit {
    #[serde(default)]
    pub monitored_ebook: Option<bool>,
    #[serde(default)]
    pub monitored_audiobook: Option<bool>,
    #[serde(default)]
    pub preferred_edition_id: Option<i64>,
}

/// `PUT /books/monitor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookMonitorRequest {
    pub book_ids: Vec<i64>,
    /// `None` = both formats.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub format: Option<BookFormat>,
    pub monitored: bool,
}

/// `GET /authors` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct AuthorListQuery {
    pub search: Option<String>,
    pub monitored: Option<bool>,
    pub sort: Option<String>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub dir: Option<SortDir>,
    pub page: Option<u32>,
    /// Omit (or 0) for all items.
    pub page_size: Option<u32>,
}

/// `GET /books` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct BookListQuery {
    pub author_id: Option<i64>,
    pub search: Option<String>,
    pub monitored: Option<bool>,
    /// Monitored books without a file of this format.
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub missing: Option<BookFormat>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

/// `GET /books/wanted` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct BookWantedQuery {
    /// `missing` (default) or `cutoff`.
    pub kind: Option<String>,
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub format: Option<BookFormat>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

/// `POST /books/search` → 202; job kind `book_search` (handled by the pipeline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookSearchRequest {
    pub kind: ContentSearchKind,
    #[serde(default)]
    pub ids: Vec<i64>,
    /// `None` = every monitored format.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub format: Option<BookFormat>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct BookNamingDto {
    pub rename: bool,
    pub author_folder: String,
    pub ebook_file: String,
    pub audiobook_folder: String,
    pub audiobook_file: String,
    pub audiobook_part_file: String,
}

impl Default for BookNamingDto {
    fn default() -> Self {
        Self {
            rename: true,
            author_folder: "{Author Name}".into(),
            ebook_file: "{Book Title}{ - Book SeriesTitle}{ Book SeriesPosition}".into(),
            audiobook_folder: "{Book Title} ({Release Year})".into(),
            audiobook_file: "{Book Title}".into(),
            audiobook_part_file: "{Book Title} - Part {Part:00}".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct CalibreSettingsDto {
    pub enabled: bool,
    pub library_path: Option<String>,
    pub calibredb_path: String,
}

impl Default for CalibreSettingsDto {
    fn default() -> Self {
        Self {
            enabled: false,
            library_path: None,
            calibredb_path: "calibredb".into(),
        }
    }
}

/// `GET/PUT /settings/books`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct BookSettingsDto {
    pub naming: BookNamingDto,
    /// Write OPF metadata / audio tags into imported files (never into seeding files).
    pub write_metadata: bool,
    pub write_covers: bool,
    pub calibre: CalibreSettingsDto,
    pub metadata_language: Option<String>,
    /// Where audiobook details (narrators, length, recording) come from.
    pub audiobook_metadata: AudiobookMetadataSettingsDto,
}

impl Default for BookSettingsDto {
    fn default() -> Self {
        Self {
            naming: BookNamingDto::default(),
            write_metadata: true,
            write_covers: true,
            calibre: CalibreSettingsDto::default(),
            metadata_language: None,
            audiobook_metadata: AudiobookMetadataSettingsDto::default(),
        }
    }
}

/// Audiobook detail sources (RFX-86). Audible and Audnexus are activated by default and asked
/// only for books monitored as audiobooks; Libro.fm is deactivated by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct AudiobookMetadataSettingsDto {
    /// Audible's public catalogue.
    pub audible: bool,
    /// Audnexus fills narrators, length and dates Audible left empty.
    pub audnexus: bool,
    /// Libro.fm's public search, when Audible has no match.
    pub librofm: bool,
    /// Audible marketplace: `us`, `uk`, `au`, `ca`, `de`, `es`, `fr`, `in`, `it` or `jp`.
    pub region: String,
}

impl Default for AudiobookMetadataSettingsDto {
    fn default() -> Self {
        Self {
            audible: true,
            audnexus: true,
            librofm: false,
            region: "us".into(),
        }
    }
}

/// `GET /books/{id}/audiobook`: one recording of a book, from the activated sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AudiobookDetailsDto {
    pub book_id: i64,
    /// False when no activated source has the book as an audiobook.
    pub found: bool,
    pub asin: Option<String>,
    pub isbn: Option<String>,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub narrators: Vec<String>,
    pub runtime_minutes: Option<u32>,
    /// True abridged, false unabridged, absent when not known.
    pub abridged: Option<bool>,
    /// Recording release date, `YYYY-MM-DD`.
    pub recording_date: Option<String>,
    pub language: Option<String>,
    pub publisher: Option<String>,
    pub series: Vec<BookSeriesRefDto>,
    pub cover_url: Option<String>,
    pub region: Option<String>,
    /// Sources that gave details, in order: `audible`, `audnexus`, `librofm`.
    pub sources: Vec<String>,
    /// The recording's public page.
    pub url: Option<String>,
    pub looked_up: DateTime<Utc>,
}

/// A folder under a book root that is not yet in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookImportCandidateDto {
    pub folder: String,
    pub path: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub kind: ContentKind,
    pub guessed_author: Option<String>,
    pub suggestions: Vec<AuthorLookupDto>,
    /// Why no suggestions could be looked up (Open Library unreachable, rate limited).
    #[serde(default)]
    pub lookup_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookImportItem {
    pub path: String,
    pub openlibrary_id: String,
    pub quality_profile_id: Option<i64>,
    pub audio_quality_profile_id: Option<i64>,
    #[serde(default)]
    pub monitor: AuthorMonitor,
    #[serde(default)]
    pub tags: Vec<i64>,
}

/// `POST /bookimport`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookLibraryImportRequest {
    pub root_folder_id: i64,
    pub items: Vec<BookImportItem>,
}

/// `GET /bookfiles?book_id=`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct BookFilesQuery {
    pub book_id: i64,
}

/// `GET /bookimport/scan?root_folder_id=`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct BookImportScanQuery {
    pub root_folder_id: i64,
    /// Look up Open Library suggestions too (default). `false` lists folders only, fast; fetch
    /// the suggestions afterwards in batches with `POST /bookimport/match`.
    #[serde(default)]
    pub lookup: Option<bool>,
}

/// `GET/POST /books/rename?author_id=`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct BookRenameQuery {
    pub author_id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_edit_distinguishes_null_from_absent() {
        let e: AuthorEdit =
            serde_json::from_str(r#"{"quality_profile_id":null,"root_folder_id":3}"#).unwrap();
        assert_eq!(e.quality_profile_id, Some(None));
        assert_eq!(e.root_folder_id, Some(Some(3)));
        assert_eq!(e.audio_quality_profile_id, None);
    }

    #[test]
    fn settings_default_when_empty() {
        let s: BookSettingsDto = serde_json::from_str("{}").unwrap();
        assert_eq!(s, BookSettingsDto::default());
        let r: AddAuthorRequest = serde_json::from_str(
            r#"{"openlibrary_id":"OL1A","quality_profile_id":1,
            "root_folder_id":2,"audio_quality_profile_id":null,"audio_root_folder_id":null}"#,
        )
        .unwrap();
        assert!(r.monitored);
        assert_eq!(r.monitor, AuthorMonitor::All);
    }
}
