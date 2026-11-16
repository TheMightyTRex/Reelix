//! Movie & series library (M2/M5).
//!
//! [`LibraryService`] owns movies, series, seasons, episodes, their files, collections and
//! root folders. Metadata comes through the [`MetadataSource`] trait (implemented for
//! [`reelix_metadata::Tmdb`]); long-running work (refresh, disk scan, rename, collection
//! sync) runs as jobs registered by [`jobs::handlers`] / [`jobs::tasks`].
//!
//! Searching and grabbing belong to the pipeline: [`LibraryService::request_search`] only
//! enqueues a `"search"` job carrying a [`reelix_api_types::library::SearchRequest`].

pub mod availability;
mod calendar;
mod collections;
pub mod content_roots;
mod convert;
pub mod custom;
mod error;
mod import;
pub mod import_api;
pub mod jobs;
pub mod metadata;
mod monitor;
mod movies;
mod refresh;
mod rename;
mod repo;
mod root_folders;
mod scan;
mod search_api;
mod series;
mod service;
mod settings;
mod stats;

pub use availability::{Availability, AvailabilityStatus};
pub use error::{LibraryError, Result};
pub use metadata::{DynMetadata, MetadataSource};
pub use reelix_metadata::MetaError;
pub use search_api::SearchApi;
pub use service::LibraryService;

/// Job kinds owned by the library.
pub mod kinds {
    pub const REFRESH_MOVIES: &str = "refresh_movies";
    pub const REFRESH_SERIES: &str = "refresh_series";
    pub const LIBRARY_SCAN: &str = "library_scan";
    pub const RENAME_FILES: &str = "rename_files";
    pub const COLLECTION_SYNC: &str = "collection_sync";
    /// Handled by the pipeline (wave 3).
    pub const SEARCH: &str = "search";
}

/// Settings keys.
pub mod keys {
    pub const NAMING: &str = "library.naming";
    pub const MEDIA_MANAGEMENT: &str = "library.media_management";
}
