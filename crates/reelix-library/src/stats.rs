//! Library statistics for the dashboard.

use reelix_api_types::library::{
    LibraryStats, MovieStats, RecentItemDto, RootFolderSpaceDto, SeriesStats, SeriesStatus,
};
use reelix_core::MediaKind;

use crate::LibraryService;
use crate::error::Result;
use crate::repo::{movies, series};

impl LibraryService {
    pub async fn stats(&self) -> Result<LibraryStats> {
        let today = self.today();
        let cache = self.profiles().await?;
        let ms = movies::load(&self.db, None, &cache).await?;
        let ss = series::load(&self.db, None, today).await?;
        let movie_stats = MovieStats {
            total: ms.len() as u64,
            monitored: ms.iter().filter(|m| m.monitored).count() as u64,
            with_file: ms.iter().filter(|m| m.has_file).count() as u64,
            missing: ms
                .iter()
                .filter(|m| m.monitored && m.is_available && !m.has_file)
                .count() as u64,
            size_on_disk: ms.iter().map(|m| m.size_on_disk).sum(),
        };
        let series_stats = SeriesStats {
            total: ss.len() as u64,
            monitored: ss.iter().filter(|s| s.monitored).count() as u64,
            continuing: ss
                .iter()
                .filter(|s| matches!(s.status, SeriesStatus::Continuing | SeriesStatus::Upcoming))
                .count() as u64,
            ended: ss.iter().filter(|s| s.status == SeriesStatus::Ended).count() as u64,
            episodes: ss
                .iter()
                .map(|s| u64::from(s.statistics.total_episode_count))
                .sum(),
            episodes_with_file: ss
                .iter()
                .map(|s| u64::from(s.statistics.episode_file_count))
                .sum(),
            missing_episodes: ss
                .iter()
                .filter(|s| s.monitored)
                .map(|s| {
                    u64::from(
                        s.statistics
                            .episode_count
                            .saturating_sub(s.statistics.episode_file_count),
                    )
                })
                .sum(),
            size_on_disk: ss.iter().map(|s| s.statistics.size_on_disk).sum(),
        };
        let mut recent: Vec<RecentItemDto> = ms
            .iter()
            .map(|m| RecentItemDto {
                kind: MediaKind::Movie,
                id: m.id,
                title: m.title.clone(),
                year: m.year,
                poster: m.images.poster.clone(),
                added: m.added,
            })
            .chain(ss.iter().map(|s| RecentItemDto {
                kind: MediaKind::Series,
                id: s.id,
                title: s.title.clone(),
                year: s.year,
                poster: s.images.poster.clone(),
                added: s.added,
            }))
            .collect();
        recent.sort_by_key(|a| std::cmp::Reverse(a.added));
        recent.truncate(10);
        let root_folders = self
            .root_folders()
            .await?
            .into_iter()
            .map(|r| RootFolderSpaceDto {
                id: r.id,
                path: r.path,
                media_kind: r.media_kind,
                free_space: r.free_space,
                total_space: r.total_space,
            })
            .collect();
        Ok(LibraryStats {
            total_size_on_disk: movie_stats.size_on_disk + series_stats.size_on_disk,
            movies: movie_stats,
            series: series_stats,
            root_folders,
            recently_added: recent,
        })
    }
}
