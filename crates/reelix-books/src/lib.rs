//! Books (M8): authors, books with ebook and audiobook formats, editions, series, files,
//! library import, download import and renaming.
//!
//! [`BooksService`] owns the book tables (migration 0008). Metadata comes through
//! [`reelix_metadata::BookMetadataProvider`] (Open Library). The pipeline searches and grabs
//! through [`BooksService::search_api`] and hands finished downloads to
//! [`BooksService::import_api`]; book root folders share `root_folders` with the library via
//! [`reelix_library::content_roots`].
//!
//! Long-running work (refresh, disk scan, rename) runs as jobs registered by
//! [`jobs::register`]; searches are only enqueued (`book_search`, handled by the pipeline).

mod authors;
mod availability;
mod books;
mod calibre;
mod custom;
mod error;
pub mod import_api;
pub mod jobs;
mod library_import;
mod metadata;
mod naming;
mod refresh;
mod rename;
mod repo;
mod root_folders;
mod scan;
pub mod search_api;
mod service;
mod settings;
pub mod tags;
#[doc(hidden)]
pub mod testing;

pub use error::{BooksError, Result};
pub use import_api::{BookImportApi, BookImportOutcome, ImportedBookFile};
pub use reelix_import::content::ContentDownload;
pub use reelix_library::{Availability, AvailabilityStatus};
pub use search_api::{BookSearchApi, BookTarget, Wanted};
pub use service::{BooksDeps, BooksService, ScanSummary};

/// Job kinds owned by the books service.
pub mod kinds {
    pub const REFRESH_AUTHORS: &str = "refresh_authors";
    pub const BOOK_SCAN: &str = "book_scan";
    pub const BOOK_RENAME: &str = "book_rename";
    /// Handled by the pipeline.
    pub const BOOK_SEARCH: &str = "book_search";
}

/// Settings keys.
pub mod keys {
    pub const SETTINGS: &str = "books.settings";
}
