//! Availability facts used by requests (wave 3 adapts these into the requests gateway).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::LibraryService;
use crate::error::Result;
use crate::repo::{movies, series};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityStatus {
    /// Every wanted file is present.
    Available,
    /// Some (but not all) aired episodes have files.
    PartiallyAvailable,
    /// In the library and monitored, nothing downloaded yet.
    Monitored,
    /// In the library, unmonitored and without files.
    Unmonitored,
    NotInLibrary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Availability {
    pub status: AvailabilityStatus,
    /// Library movie/series id.
    pub library_id: Option<i64>,
    /// Per season (series only).
    pub seasons: BTreeMap<u32, AvailabilityStatus>,
}

fn status(files: u32, expected: u32, total: u32, monitored: bool) -> AvailabilityStatus {
    if files > 0 && files >= expected.max(1) && (expected > 0 || files >= total) {
        AvailabilityStatus::Available
    } else if files > 0 {
        AvailabilityStatus::PartiallyAvailable
    } else if monitored {
        AvailabilityStatus::Monitored
    } else {
        AvailabilityStatus::Unmonitored
    }
}

impl LibraryService {
    /// Availability per TMDB movie id (ids not in the library → `NotInLibrary`).
    pub async fn movie_availability(&self, tmdb_ids: &[u32]) -> Result<HashMap<u32, Availability>> {
        let index = movies::tmdb_index(&self.db).await?;
        let ids: Vec<i64> = tmdb_ids.iter().filter_map(|t| index.get(t).copied()).collect();
        let cache = self.profiles().await?;
        let list = movies::load(&self.db, Some(&ids), &cache).await?;
        let mut out = HashMap::new();
        for t in tmdb_ids {
            let m = list.iter().find(|m| m.tmdb_id == *t);
            out.insert(
                *t,
                Availability {
                    status: match m {
                        None => AvailabilityStatus::NotInLibrary,
                        Some(m) if m.has_file => AvailabilityStatus::Available,
                        Some(m) if m.monitored => AvailabilityStatus::Monitored,
                        Some(_) => AvailabilityStatus::Unmonitored,
                    },
                    library_id: m.map(|m| m.id),
                    seasons: BTreeMap::new(),
                },
            );
        }
        Ok(out)
    }

    /// Availability per TMDB series id, with per-season detail.
    pub async fn series_availability(&self, tmdb_ids: &[u32]) -> Result<HashMap<u32, Availability>> {
        let (index, _) = series::id_index(&self.db).await?;
        let ids: Vec<i64> = tmdb_ids.iter().filter_map(|t| index.get(t).copied()).collect();
        let list = series::load(&self.db, Some(&ids), self.today()).await?;
        let mut out = HashMap::new();
        for t in tmdb_ids {
            let s = list.iter().find(|s| s.tmdb_id == Some(*t));
            let a = match s {
                None => Availability {
                    status: AvailabilityStatus::NotInLibrary,
                    library_id: None,
                    seasons: BTreeMap::new(),
                },
                Some(s) => {
                    let seasons = s
                        .seasons
                        .iter()
                        .filter(|x| x.season_number > 0)
                        .map(|x| {
                            let st = &x.statistics;
                            (
                                x.season_number,
                                status(
                                    st.episode_file_count,
                                    st.episode_count,
                                    st.total_episode_count,
                                    x.monitored,
                                ),
                            )
                        })
                        .collect();
                    let st = &s.statistics;
                    Availability {
                        status: status(
                            st.episode_file_count,
                            st.episode_count,
                            st.total_episode_count,
                            s.monitored,
                        ),
                        library_id: Some(s.id),
                        seasons,
                    }
                }
            };
            out.insert(*t, a);
        }
        Ok(out)
    }
}
