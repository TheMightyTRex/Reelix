//! Provider-neutral book metadata (authors, works, editions) and the [`BookMetadataProvider`]
//! trait implemented by [`OpenLibrary`](crate::OpenLibrary).

use async_trait::async_trait;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::MetaError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AuthorMetadata {
    /// Provider id, e.g. Open Library `OL23919A`.
    pub id: String,
    pub name: String,
    pub sort_name: String,
    pub bio: Option<String>,
    pub birth_date: Option<String>,
    pub death_date: Option<String>,
    pub photo_url: Option<String>,
    pub links: Vec<String>,
    pub alternate_names: Vec<String>,
}

/// An author credited on a work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorkAuthor {
    pub id: String,
    pub name: String,
    /// `author`, `narrator`, `translator`, `editor`…
    pub role: Option<String>,
}

/// A series a work or edition belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SeriesRef {
    pub title: String,
    /// Display position, e.g. `1`, `2.5`.
    pub position: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct EditionMetadata {
    /// Provider id, e.g. Open Library `OL7353617M`.
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub isbn13: Option<String>,
    pub isbn10: Option<String>,
    pub asin: Option<String>,
    pub publishers: Vec<String>,
    pub publish_date: Option<String>,
    pub language: Option<String>,
    pub page_count: Option<u32>,
    pub physical_format: Option<String>,
    pub is_audiobook: bool,
    pub cover_url: Option<String>,
    pub series: Vec<SeriesRef>,
    pub narrators: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorkMetadata {
    /// Provider id, e.g. Open Library `OL45883W`.
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub authors: Vec<WorkAuthor>,
    pub first_publish_date: Option<NaiveDate>,
    pub first_publish_year: Option<i32>,
    pub subjects: Vec<String>,
    pub cover_url: Option<String>,
    pub series: Vec<SeriesRef>,
    pub rating: Option<f64>,
    pub rating_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AuthorSearchHit {
    pub id: String,
    pub name: String,
    pub birth_date: Option<String>,
    pub top_work: Option<String>,
    pub work_count: u32,
    pub photo_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorkSearchHit {
    pub id: String,
    pub title: String,
    pub author_ids: Vec<String>,
    pub author_names: Vec<String>,
    pub first_publish_year: Option<i32>,
    pub cover_url: Option<String>,
    pub isbns: Vec<String>,
    pub edition_count: u32,
}

/// Book metadata source used by the books service.
#[async_trait]
pub trait BookMetadataProvider: Send + Sync {
    /// Stable implementation id (`openlibrary`).
    fn name(&self) -> &'static str;
    async fn search_authors(&self, q: &str) -> Result<Vec<AuthorSearchHit>, MetaError>;
    async fn search_works(&self, q: &str) -> Result<Vec<WorkSearchHit>, MetaError>;
    async fn author(&self, id: &str) -> Result<AuthorMetadata, MetaError>;
    /// Every work of the author (paged internally).
    async fn author_works(&self, id: &str) -> Result<Vec<WorkMetadata>, MetaError>;
    async fn work(&self, id: &str) -> Result<WorkMetadata, MetaError>;
    async fn work_editions(&self, work_id: &str) -> Result<Vec<EditionMetadata>, MetaError>;
    /// ISBN-10/13 → (work, edition).
    async fn by_isbn(&self, isbn: &str) -> Result<(WorkMetadata, EditionMetadata), MetaError>;
}
