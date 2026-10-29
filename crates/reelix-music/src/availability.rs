//! Availability of albums for requests.

use std::collections::{BTreeMap, HashMap};

use reelix_library::{Availability, AvailabilityStatus};

use crate::error::Result;
use crate::{MusicService, repo};

impl MusicService {
    /// Availability per MusicBrainz release-group id (unknown ids are absent).
    pub async fn availability(&self, release_group_ids: &[String]) -> Result<HashMap<String, Availability>> {
        let mut out = HashMap::new();
        for rg in release_group_ids {
            let Some(a) = repo::albums::by_mbid(&self.db, rg).await? else {
                continue;
            };
            let s = &a.statistics;
            let status = if s.track_count > 0 && s.track_file_count >= s.track_count {
                AvailabilityStatus::Available
            } else if s.track_file_count > 0 || s.size_on_disk > 0 {
                AvailabilityStatus::PartiallyAvailable
            } else if a.monitored {
                AvailabilityStatus::Monitored
            } else {
                AvailabilityStatus::Unmonitored
            };
            out.insert(
                rg.clone(),
                Availability {
                    status,
                    library_id: Some(a.id),
                    seasons: BTreeMap::new(),
                },
            );
        }
        Ok(out)
    }
}
