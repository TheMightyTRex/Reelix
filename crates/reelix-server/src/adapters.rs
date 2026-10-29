//! Adapters that connect domain crates which must not depend on each other.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use reelix_api_types::library::{
    AddMovieRequest as LibAddMovie, AddSeriesRequest as LibAddSeries, LibrarySearchKind, MinimumAvailability,
    SearchRequest, SeasonMonitorRequest, SeriesType,
};
use reelix_api_types::requests::{AvailabilityStatus, RootFolderOptionDto};
use reelix_core::MediaKind;
use reelix_db::Db;
use reelix_library::{Availability as LibAvailability, AvailabilityStatus as LibStatus, LibraryService};
use reelix_requests::gateway::{AddMovieRequest, AddSeriesRequest, Availability, LibraryGateway};

/// [`LibraryGateway`] backed by the real [`LibraryService`].
pub struct LibraryAdapter {
    pub library: Arc<LibraryService>,
    pub db: Db,
    /// Book requests.
    pub books: Arc<reelix_books::BooksService>,
    /// Album requests.
    pub music: Arc<reelix_music::MusicService>,
}

fn status(s: LibStatus) -> AvailabilityStatus {
    match s {
        LibStatus::Available => AvailabilityStatus::Available,
        LibStatus::PartiallyAvailable => AvailabilityStatus::PartiallyAvailable,
        LibStatus::Monitored => AvailabilityStatus::Monitored,
        // Unmonitored items are treated as not requested, so they can be requested again.
        LibStatus::Unmonitored | LibStatus::NotInLibrary => AvailabilityStatus::Unknown,
    }
}

fn availability(a: LibAvailability) -> Availability {
    Availability {
        status: status(a.status),
        library_id: a.library_id,
        seasons: a.seasons.into_iter().map(|(n, s)| (n, status(s))).collect(),
    }
}

impl LibraryAdapter {
    async fn default_profile(&self, requested: Option<i64>) -> anyhow::Result<i64> {
        if let Some(id) = requested {
            return Ok(id);
        }
        let profiles = self
            .db
            .quality_profiles()
            .list()
            .await
            .map_err(|e| anyhow!("{e}"))?;
        profiles
            .first()
            .map(|p| p.id)
            .context("no quality profile exists; create one under Settings → Profiles")
    }

    async fn default_root(&self, kind: MediaKind, requested: Option<i64>) -> anyhow::Result<i64> {
        if let Some(id) = requested {
            return Ok(id);
        }
        let roots = self.library.root_folders().await.map_err(|e| anyhow!("{e}"))?;
        roots
            .into_iter()
            .find(|r| r.media_kind == kind)
            .map(|r| r.id)
            .context("no root folder for this media type; add one under Settings → Media Management")
    }
}

#[async_trait]
impl LibraryGateway for LibraryAdapter {
    async fn availability(
        &self,
        kind: MediaKind,
        tmdb_ids: &[u32],
    ) -> anyhow::Result<HashMap<u32, Availability>> {
        let map = match kind {
            MediaKind::Movie => self.library.movie_availability(tmdb_ids).await,
            MediaKind::Series => self.library.series_availability(tmdb_ids).await,
        }
        .map_err(|e| anyhow!("{e}"))?;
        Ok(map.into_iter().map(|(id, a)| (id, availability(a))).collect())
    }

    async fn add_movie(&self, req: AddMovieRequest) -> anyhow::Result<i64> {
        // Already in the library (e.g. unmonitored): reuse it rather than failing on a duplicate.
        let existing = self
            .library
            .movie_availability(&[req.tmdb_id])
            .await
            .map_err(|e| anyhow!("{e}"))?;
        if let Some(id) = existing.get(&req.tmdb_id).and_then(|a| a.library_id) {
            return Ok(id);
        }
        let minimum_availability = match req.minimum_availability.as_str() {
            "announced" => MinimumAvailability::Announced,
            "in_cinemas" | "incinemas" => MinimumAvailability::InCinemas,
            _ => MinimumAvailability::Released,
        };
        let movie = self
            .library
            .add_movie(LibAddMovie {
                tmdb_id: req.tmdb_id,
                quality_profile_id: self.default_profile(req.quality_profile_id).await?,
                root_folder_id: self.default_root(MediaKind::Movie, req.root_folder_id).await?,
                monitor: Default::default(),
                minimum_availability,
                tags: req.tags,
                search_on_add: false,
                path: None,
                added_by_request_id: Some(req.request_id),
            })
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(movie.id)
    }

    async fn add_series(&self, req: AddSeriesRequest) -> anyhow::Result<i64> {
        // Existing series: monitor the newly requested seasons instead of adding again.
        if let Some(series) = self
            .library
            .series_by_tmdb(req.tmdb_id)
            .await
            .map_err(|e| anyhow!("{e}"))?
        {
            for season_number in &req.seasons {
                self.library
                    .monitor_season(SeasonMonitorRequest {
                        series_id: series.id,
                        season_number: *season_number,
                        monitored: true,
                        include_episodes: true,
                    })
                    .await
                    .map_err(|e| anyhow!("{e}"))?;
            }
            return Ok(series.id);
        }
        let series_type = match req.series_type.as_str() {
            "daily" => SeriesType::Daily,
            "anime" => SeriesType::Anime,
            _ => SeriesType::Standard,
        };
        let series = self
            .library
            .add_series(LibAddSeries {
                tmdb_id: Some(req.tmdb_id),
                tvdb_id: req.tvdb_id,
                quality_profile_id: self.default_profile(req.quality_profile_id).await?,
                root_folder_id: self.default_root(MediaKind::Series, req.root_folder_id).await?,
                monitor: Default::default(),
                seasons: (!req.seasons.is_empty()).then_some(req.seasons),
                series_type,
                season_folder: req.season_folder,
                monitor_new_items: Default::default(),
                tags: req.tags,
                search_on_add: false,
                search_cutoff_unmet: false,
                path: None,
                added_by_request_id: Some(req.request_id),
            })
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(series.id)
    }

    async fn request_search(&self, kind: MediaKind, ids: &[i64]) -> anyhow::Result<()> {
        let kind = match kind {
            MediaKind::Movie => LibrarySearchKind::Movie,
            MediaKind::Series => LibrarySearchKind::Series,
        };
        self.library
            .request_search(SearchRequest {
                kind,
                ids: ids.to_vec(),
                season: None,
                media: None,
            })
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    async fn root_folders(&self, kind: MediaKind) -> anyhow::Result<Vec<RootFolderOptionDto>> {
        let roots = self.library.root_folders().await.map_err(|e| anyhow!("{e}"))?;
        Ok(roots
            .into_iter()
            .filter(|r| r.media_kind == kind)
            .map(|r| RootFolderOptionDto {
                id: r.id,
                path: r.path,
                free_space: r.free_space,
            })
            .collect())
    }
}
