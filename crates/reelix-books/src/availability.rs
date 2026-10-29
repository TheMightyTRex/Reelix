//! Availability per Open Library work id (used by requests).

use std::collections::{BTreeMap, HashMap};

use reelix_core::BookFormat;
use reelix_library::{Availability, AvailabilityStatus};

use crate::BooksService;
use crate::error::Result;
use crate::repo::books::BookFilter;
use crate::repo::{authors, books as repo};
use crate::service::{file_for, monitored_for, profile_of};

impl BooksService {
    /// Availability per Open Library work id (unknown ids are absent). A book is available
    /// when every monitored format the author wants has a file, partially available when
    /// some do.
    pub async fn availability(&self, work_ids: &[String]) -> Result<HashMap<String, Availability>> {
        let index = repo::work_index(&self.db).await?;
        let ids: Vec<i64> = work_ids
            .iter()
            .filter_map(|w| index.get(w.trim()).copied())
            .collect();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let books = self
            .load_books(BookFilter {
                ids: Some(&ids),
                author_ids: None,
            })
            .await?;
        let authors: HashMap<i64, _> = authors::load(&self.db, None)
            .await?
            .into_iter()
            .map(|a| (a.id, a))
            .collect();
        let mut out = HashMap::new();
        for b in books {
            let Some(a) = authors.get(&b.author_id) else {
                continue;
            };
            let wanted: Vec<BookFormat> = BookFormat::ALL
                .into_iter()
                .filter(|f| profile_of(a, *f).is_some() && monitored_for(&b, *f))
                .collect();
            let have = BookFormat::ALL
                .into_iter()
                .filter(|f| file_for(&b, *f).is_some())
                .count();
            let have_wanted = wanted.iter().filter(|f| file_for(&b, **f).is_some()).count();
            let status = if have > 0 && have_wanted == wanted.len() {
                AvailabilityStatus::Available
            } else if have > 0 {
                AvailabilityStatus::PartiallyAvailable
            } else if !wanted.is_empty() && a.monitored {
                AvailabilityStatus::Monitored
            } else {
                AvailabilityStatus::Unmonitored
            };
            out.insert(
                b.openlibrary_work_id.clone(),
                Availability {
                    status,
                    library_id: Some(b.id),
                    seasons: BTreeMap::new(),
                },
            );
        }
        Ok(out)
    }
}
