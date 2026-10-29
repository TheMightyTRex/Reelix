//! Episode/season monitoring: monitor options, season monitoring and the season pass.

use std::collections::BTreeSet;

use reelix_api_types::library::{
    EpisodeDto, EpisodeMonitorRequest, SeasonMonitorRequest, SeasonPassRequest, SeriesDto, SeriesMonitor,
};

use crate::LibraryService;
use crate::error::{LibraryError, Result};
use crate::repo::series as repo;

/// Which episodes `monitor` selects (specials are never auto-monitored unless listed in `seasons`).
pub(crate) fn select_monitored(
    episodes: &[EpisodeDto],
    monitor: SeriesMonitor,
    seasons: Option<&[u32]>,
    today: chrono::NaiveDate,
) -> Vec<bool> {
    let regular: BTreeSet<u32> = episodes
        .iter()
        .map(|e| e.season_number)
        .filter(|s| *s > 0)
        .collect();
    let first = regular.iter().next().copied();
    let latest = regular.iter().next_back().copied();
    let future = |e: &EpisodeDto| e.air_date.is_none_or(|d| d > today);
    episodes
        .iter()
        .map(|e| {
            if let Some(list) = seasons {
                return list.contains(&e.season_number);
            }
            if e.season_number == 0 {
                return false;
            }
            match monitor {
                SeriesMonitor::All => true,
                SeriesMonitor::Future => future(e),
                SeriesMonitor::Missing => !e.has_file || future(e),
                SeriesMonitor::Existing => e.has_file,
                SeriesMonitor::FirstSeason => Some(e.season_number) == first,
                SeriesMonitor::LatestSeason => Some(e.season_number) == latest,
                SeriesMonitor::Pilot => Some(e.season_number) == first && e.episode_number == 1,
                SeriesMonitor::None => false,
            }
        })
        .collect()
}

impl LibraryService {
    /// Applies a monitor option (or an explicit season list) to every episode and season.
    pub(crate) async fn apply_monitor(
        &self,
        series_id: i64,
        monitor: SeriesMonitor,
        seasons: Option<&[u32]>,
    ) -> Result<()> {
        let eps = repo::episodes(&self.db, series_id, None).await?;
        let flags = select_monitored(&eps, monitor, seasons, self.today());
        let (on, off): (Vec<_>, Vec<_>) = eps.iter().zip(&flags).partition(|(_, f)| **f);
        repo::set_episodes_monitored(&self.db, &on.iter().map(|(e, _)| e.id).collect::<Vec<_>>(), true)
            .await?;
        repo::set_episodes_monitored(
            &self.db,
            &off.iter().map(|(e, _)| e.id).collect::<Vec<_>>(),
            false,
        )
        .await?;
        let s = self.series_row(series_id).await?;
        for season in &s.seasons {
            let monitored = match seasons {
                Some(list) => list.contains(&season.season_number),
                None => {
                    eps.iter()
                        .zip(&flags)
                        .any(|(e, f)| *f && e.season_number == season.season_number)
                        || (season.season_number > 0
                            && matches!(
                                monitor,
                                SeriesMonitor::All | SeriesMonitor::Future | SeriesMonitor::Missing
                            )
                            && !eps.iter().any(|e| e.season_number == season.season_number))
                }
            };
            repo::set_season_monitored(&self.db, series_id, season.season_number, monitored).await?;
        }
        Ok(())
    }

    /// (Un)monitors episodes; returns the updated episodes.
    pub async fn monitor_episodes(&self, req: EpisodeMonitorRequest) -> Result<Vec<EpisodeDto>> {
        repo::set_episodes_monitored(&self.db, &req.episode_ids, req.monitored).await?;
        let eps = repo::episodes_by_ids(&self.db, &req.episode_ids).await?;
        let mut series = BTreeSet::new();
        for e in &eps {
            self.emit("episode", "updated", e.id);
            series.insert(e.series_id);
        }
        for s in series {
            self.emit("series", "updated", s);
        }
        Ok(eps)
    }

    /// (Un)monitors a season and (by default) its episodes.
    pub async fn monitor_season(&self, req: SeasonMonitorRequest) -> Result<SeriesDto> {
        self.series_row(req.series_id).await?;
        if !repo::set_season_monitored(&self.db, req.series_id, req.season_number, req.monitored).await? {
            return Err(LibraryError::NotFound(format!(
                "season {} of series {} not found",
                req.season_number, req.series_id
            )));
        }
        if req.include_episodes {
            let ids: Vec<i64> = repo::episodes(&self.db, req.series_id, Some(req.season_number))
                .await?
                .iter()
                .map(|e| e.id)
                .collect();
            repo::set_episodes_monitored(&self.db, &ids, req.monitored).await?;
        }
        self.emit("series", "updated", req.series_id);
        self.series_row(req.series_id).await
    }

    /// Applies `monitor` to many series.
    pub async fn season_pass(&self, req: SeasonPassRequest) -> Result<Vec<SeriesDto>> {
        let mut out = Vec::new();
        for id in &req.series_ids {
            let mut s = self.series_row(*id).await?;
            if let Some(m) = req.monitored {
                s.monitored = m;
                repo::save(&self.db, s.id, &s).await?;
            }
            self.apply_monitor(*id, req.monitor, None).await?;
            self.emit("series", "updated", *id);
            out.push(self.series_row(*id).await?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn ep(season: u32, n: u32, air: Option<&str>, has_file: bool) -> EpisodeDto {
        EpisodeDto {
            id: (season * 100 + n) as i64,
            series_id: 1,
            season_number: season,
            episode_number: n,
            absolute_number: None,
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_number: None,
            title: None,
            overview: None,
            air_date: air.map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap()),
            air_date_utc: None,
            runtime_minutes: None,
            still: None,
            finale: None,
            monitored: false,
            has_file,
            episode_file_id: None,
            episode_file: None,
        }
    }

    #[test]
    fn monitor_options() {
        let today = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
        let eps = vec![
            ep(0, 1, Some("2020-01-01"), false),
            ep(1, 1, Some("2020-01-01"), true),
            ep(1, 2, Some("2020-01-08"), false),
            ep(2, 1, Some("2025-01-01"), false),
            ep(2, 2, None, false),
        ];
        let f = |m| select_monitored(&eps, m, None, today);
        assert_eq!(f(SeriesMonitor::All), vec![false, true, true, true, true]);
        assert_eq!(f(SeriesMonitor::Future), vec![false, false, false, true, true]);
        assert_eq!(f(SeriesMonitor::Missing), vec![false, false, true, true, true]);
        assert_eq!(f(SeriesMonitor::Existing), vec![false, true, false, false, false]);
        assert_eq!(
            f(SeriesMonitor::FirstSeason),
            vec![false, true, true, false, false]
        );
        assert_eq!(
            f(SeriesMonitor::LatestSeason),
            vec![false, false, false, true, true]
        );
        assert_eq!(f(SeriesMonitor::Pilot), vec![false, true, false, false, false]);
        assert_eq!(f(SeriesMonitor::None), vec![false; 5]);
        assert_eq!(
            select_monitored(&eps, SeriesMonitor::None, Some(&[0, 2]), today),
            vec![true, false, false, true, true]
        );
    }
}
