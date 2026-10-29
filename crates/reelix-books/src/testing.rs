//! An in-memory [`BookMetadataProvider`] for tests (this crate's and the API's).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use reelix_metadata::{
    AuthorMetadata, AuthorSearchHit, BookMetadataProvider, EditionMetadata, MetaError, WorkAuthor,
    WorkMetadata, WorkSearchHit,
};

/// Fixed authors, works and editions; unknown ids are `MetaError::NotFound`.
#[derive(Default)]
pub struct FakeBookMetadata {
    inner: Mutex<Data>,
}

#[derive(Default)]
struct Data {
    authors: Vec<AuthorMetadata>,
    works: Vec<WorkMetadata>,
    editions: HashMap<String, Vec<EditionMetadata>>,
    calls: Vec<String>,
}

/// A work credited to one author.
pub fn work(id: &str, title: &str, author: &AuthorMetadata, year: Option<i32>) -> WorkMetadata {
    WorkMetadata {
        id: id.into(),
        title: title.into(),
        authors: vec![WorkAuthor {
            id: author.id.clone(),
            name: author.name.clone(),
            role: None,
        }],
        first_publish_year: year,
        first_publish_date: year.and_then(|y| chrono::NaiveDate::from_ymd_opt(y, 1, 1)),
        ..Default::default()
    }
}

pub fn author(id: &str, name: &str) -> AuthorMetadata {
    AuthorMetadata {
        id: id.into(),
        name: name.into(),
        ..Default::default()
    }
}

impl FakeBookMetadata {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_author(self, a: AuthorMetadata) -> Self {
        self.lock().authors.push(a);
        self
    }

    pub fn with_work(self, w: WorkMetadata) -> Self {
        self.lock().works.push(w);
        self
    }

    pub fn with_editions(self, work_id: &str, e: Vec<EditionMetadata>) -> Self {
        self.lock().editions.insert(work_id.into(), e);
        self
    }

    /// Adds or replaces a work after construction (refresh tests).
    pub fn put_work(&self, w: WorkMetadata) {
        let mut d = self.lock();
        d.works.retain(|x| x.id != w.id);
        d.works.push(w);
    }

    /// Every call so far, e.g. `"author_works:OL1A"`.
    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Data> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(&self, c: String) {
        self.lock().calls.push(c);
    }
}

#[async_trait]
impl BookMetadataProvider for FakeBookMetadata {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn search_authors(&self, q: &str) -> Result<Vec<AuthorSearchHit>, MetaError> {
        self.record(format!("search_authors:{q}"));
        let q = q.to_lowercase();
        let d = self.lock();
        Ok(d.authors
            .iter()
            .filter(|a| a.name.to_lowercase().contains(&q) || q.contains(&a.name.to_lowercase()))
            .map(|a| AuthorSearchHit {
                id: a.id.clone(),
                name: a.name.clone(),
                birth_date: a.birth_date.clone(),
                top_work: d
                    .works
                    .iter()
                    .find(|w| w.authors.iter().any(|x| x.id == a.id))
                    .map(|w| w.title.clone()),
                work_count: d
                    .works
                    .iter()
                    .filter(|w| w.authors.iter().any(|x| x.id == a.id))
                    .count() as u32,
                photo_url: a.photo_url.clone(),
            })
            .collect())
    }

    async fn search_works(&self, q: &str) -> Result<Vec<WorkSearchHit>, MetaError> {
        self.record(format!("search_works:{q}"));
        let q = q.to_lowercase();
        Ok(self
            .lock()
            .works
            .iter()
            .filter(|w| w.title.to_lowercase().contains(&q))
            .map(|w| WorkSearchHit {
                id: w.id.clone(),
                title: w.title.clone(),
                author_ids: w.authors.iter().map(|a| a.id.clone()).collect(),
                author_names: w.authors.iter().map(|a| a.name.clone()).collect(),
                first_publish_year: w.first_publish_year,
                cover_url: w.cover_url.clone(),
                isbns: Vec::new(),
                edition_count: 0,
            })
            .collect())
    }

    async fn author(&self, id: &str) -> Result<AuthorMetadata, MetaError> {
        self.record(format!("author:{id}"));
        self.lock()
            .authors
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or(MetaError::NotFound)
    }

    async fn author_works(&self, id: &str) -> Result<Vec<WorkMetadata>, MetaError> {
        self.record(format!("author_works:{id}"));
        Ok(self
            .lock()
            .works
            .iter()
            .filter(|w| w.authors.first().is_some_and(|a| a.id == id))
            .cloned()
            .collect())
    }

    async fn work(&self, id: &str) -> Result<WorkMetadata, MetaError> {
        self.record(format!("work:{id}"));
        self.lock()
            .works
            .iter()
            .find(|w| w.id == id)
            .cloned()
            .ok_or(MetaError::NotFound)
    }

    async fn work_editions(&self, work_id: &str) -> Result<Vec<EditionMetadata>, MetaError> {
        self.record(format!("work_editions:{work_id}"));
        Ok(self.lock().editions.get(work_id).cloned().unwrap_or_default())
    }

    async fn by_isbn(&self, isbn: &str) -> Result<(WorkMetadata, EditionMetadata), MetaError> {
        self.record(format!("by_isbn:{isbn}"));
        let d = self.lock();
        for (work_id, eds) in &d.editions {
            if let Some(e) = eds
                .iter()
                .find(|e| e.isbn13.as_deref() == Some(isbn) || e.isbn10.as_deref() == Some(isbn))
                && let Some(w) = d.works.iter().find(|w| &w.id == work_id)
            {
                return Ok((w.clone(), e.clone()));
            }
        }
        Err(MetaError::NotFound)
    }
}
