//! Another numbering of a series' episodes, mapped to TMDB's.
//!
//! Reelix numbers episodes as TMDB does, but release and file names often carry TVDB or
//! broadcast numbers, which can differ (TMDB's season 3 of Archer has 10 episodes, TVDB's 13).
//! TMDB's episode groups give the aired order of many such series; the library keeps the one
//! it picks as an [`EpisodeOrderMap`] per series, and [`match_by_episode_map`] uses it for a
//! file whose numbers TMDB's own numbering cannot place.

use serde::{Deserialize, Serialize};

use reelix_parser::model::EpisodeInfo;

use crate::matching::ExpectedEpisode;

/// One episode: its number in the other order and its TMDB number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeOrderEntry {
    pub season: u32,
    pub episode: u32,
    pub tmdb_season: u32,
    pub tmdb_episode: u32,
}

/// A series' episodes in another order (TMDB's episode group), mapped to TMDB numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeOrderMap {
    /// TMDB episode group id.
    pub group_id: String,
    /// TMDB episode group name ("Aired Order", "TVDB Order" and so on).
    pub group_name: String,
    pub entries: Vec<EpisodeOrderEntry>,
}

impl EpisodeOrderMap {
    /// A map from `(season, episode, tmdb_season, tmdb_episode)` tuples. `None` when it would
    /// change nothing (every episode has the same numbers in both orders), so nothing is kept.
    pub fn new(
        group_id: impl Into<String>,
        group_name: impl Into<String>,
        numbering: impl IntoIterator<Item = (u32, u32, u32, u32)>,
    ) -> Option<Self> {
        let mut entries: Vec<EpisodeOrderEntry> = Vec::new();
        for (season, episode, tmdb_season, tmdb_episode) in numbering {
            // The first entry for a number wins; a group that lists one twice is mistaken.
            if entries.iter().any(|e| e.season == season && e.episode == episode) {
                continue;
            }
            entries.push(EpisodeOrderEntry {
                season,
                episode,
                tmdb_season,
                tmdb_episode,
            });
        }
        if entries
            .iter()
            .all(|e| e.season == e.tmdb_season && e.episode == e.tmdb_episode)
        {
            return None;
        }
        Some(Self {
            group_id: group_id.into(),
            group_name: group_name.into(),
            entries,
        })
    }

    /// TMDB's `(season, episode)` for an episode numbered in this order.
    pub fn tmdb_for(&self, season: u32, episode: u32) -> Option<(u32, u32)> {
        self.entries
            .iter()
            .find(|e| e.season == season && e.episode == episode)
            .map(|e| (e.tmdb_season, e.tmdb_episode))
    }
}

/// The episodes a parsed `SxxEyy` name points at when its numbers are read in the map's order.
/// Every number must be in the map and its TMDB episode in `episodes`, otherwise `None`. Names
/// with only a date, absolute numbers or a whole season are not numbered in a season order, so
/// they are left to the usual matching.
pub fn match_by_episode_map(
    info: &EpisodeInfo,
    map: &EpisodeOrderMap,
    episodes: &[ExpectedEpisode],
) -> Option<Vec<i64>> {
    let season = info.season?;
    if info.episodes.is_empty() || info.season_range.is_some() {
        return None;
    }
    let mut ids = Vec::with_capacity(info.episodes.len());
    for ep in &info.episodes {
        let (ts, te) = map.tmdb_for(season, *ep)?;
        let found = episodes.iter().find(|e| e.season == ts && e.episode == te)?;
        if !ids.contains(&found.id) {
            ids.push(found.id);
        }
    }
    Some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Archer's season 3 as TVDB numbers it (13 episodes) against TMDB's 10, with the last
    /// three in TMDB's season 4.
    fn archer() -> EpisodeOrderMap {
        let mut n: Vec<(u32, u32, u32, u32)> = (1..=10).map(|e| (3, e, 3, e)).collect();
        n.extend([(3, 11, 4, 1), (3, 12, 4, 2), (3, 13, 4, 3)]);
        n.extend((4..=16).map(|e| (4, e - 3, 4, e)));
        EpisodeOrderMap::new("5b1f", "Aired Order", n).unwrap()
    }

    fn eps() -> Vec<ExpectedEpisode> {
        let mut v = Vec::new();
        for (season, count) in [(3, 10), (4, 16)] {
            for e in 1..=count {
                v.push(ExpectedEpisode {
                    id: i64::from(season * 100 + e),
                    season,
                    episode: e,
                    absolute: None,
                    air_date: None,
                    scene_season: None,
                    scene_episode: None,
                    scene_absolute: None,
                });
            }
        }
        v
    }

    fn info(season: u32, episodes: &[u32]) -> EpisodeInfo {
        EpisodeInfo {
            season: Some(season),
            episodes: episodes.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn same_numbers_keep_nothing() {
        assert_eq!(
            EpisodeOrderMap::new("g", "Aired", [(1, 1, 1, 1), (1, 2, 1, 2)]),
            None
        );
        assert_eq!(EpisodeOrderMap::new("g", "Aired", []), None);
    }

    #[test]
    fn first_entry_for_a_number_wins() {
        let m = EpisodeOrderMap::new("g", "Aired", [(1, 1, 1, 2), (1, 1, 1, 3)]).unwrap();
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.tmdb_for(1, 1), Some((1, 2)));
    }

    #[test]
    fn other_numbers_map_to_tmdb_episodes() {
        let m = archer();
        assert_eq!(m.tmdb_for(3, 11), Some((4, 1)));
        assert_eq!(match_by_episode_map(&info(3, &[11]), &m, &eps()), Some(vec![401]));
        assert_eq!(
            match_by_episode_map(&info(3, &[12, 13]), &m, &eps()),
            Some(vec![402, 403])
        );
        assert_eq!(match_by_episode_map(&info(4, &[1]), &m, &eps()), Some(vec![404]));
        assert_eq!(match_by_episode_map(&info(3, &[5]), &m, &eps()), Some(vec![305]));
    }

    #[test]
    fn unknown_numbers_and_other_names_do_not_match() {
        let m = archer();
        assert_eq!(match_by_episode_map(&info(3, &[14]), &m, &eps()), None);
        // One number outside the map spoils a multi-episode name.
        assert_eq!(match_by_episode_map(&info(3, &[13, 14]), &m, &eps()), None);
        let whole = EpisodeInfo {
            season: Some(3),
            full_season: true,
            ..Default::default()
        };
        assert_eq!(match_by_episode_map(&whole, &m, &eps()), None);
        let absolute = EpisodeInfo {
            absolute_episodes: vec![30],
            ..Default::default()
        };
        assert_eq!(match_by_episode_map(&absolute, &m, &eps()), None);
    }

    #[test]
    fn a_tmdb_episode_the_series_lacks_does_not_match() {
        let m = EpisodeOrderMap::new("g", "Aired", [(1, 1, 9, 9)]).unwrap();
        assert_eq!(match_by_episode_map(&info(1, &[1]), &m, &eps()), None);
    }
}
