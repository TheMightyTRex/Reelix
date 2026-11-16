//! Read helpers the pipeline's search stage uses (decision context, release mapping, wanted lists).
//!
//! Everything hangs off [`LibraryService::search_api`]: bulk loads of movies / series /
//! episodes with their files (for decision contexts and RSS mapping) without per-item
//! round trips through the API-shaped methods.

use std::collections::HashMap;

use chrono::NaiveDate;
use reelix_api_types::library::{EpisodeDto, EpisodeFileDto, MovieDto, SeriesDto};

use crate::LibraryService;
use crate::error::Result;
use crate::repo::{movies, placeholders, series as repo};

/// Read-only library access for searching.
pub struct SearchApi<'a> {
    svc: &'a LibraryService,
}

impl LibraryService {
    pub fn search_api(&self) -> SearchApi<'_> {
        SearchApi { svc: self }
    }
}

impl SearchApi<'_> {
    /// Library "today" (overridable in tests).
    pub fn today(&self) -> NaiveDate {
        self.svc.today()
    }

    /// Movies (all, or `ids`) with tags, alternative titles and primary file.
    pub async fn movies(&self, ids: Option<&[i64]>) -> Result<Vec<MovieDto>> {
        let cache = self.svc.profiles().await?;
        movies::load(&self.svc.db, ids, &cache).await
    }

    /// Series (all, or `ids`) with seasons, tags and alternative titles.
    pub async fn series(&self, ids: Option<&[i64]>) -> Result<Vec<SeriesDto>> {
        repo::load(&self.svc.db, ids, self.svc.today()).await
    }

    /// Every episode of a series (optionally one season) with its file attached.
    pub async fn episodes_of(&self, s: &SeriesDto, season: Option<u32>) -> Result<Vec<EpisodeDto>> {
        let mut eps = repo::episodes(&self.svc.db, s.id, season).await?;
        self.attach_files(s, &mut eps).await?;
        Ok(eps)
    }

    /// [`Self::episodes_of`] (whole series) for many series at once, by series id. The same episodes and
    /// files, read in a few queries instead of about five for each series.
    pub async fn episodes_of_many(&self, series: &[SeriesDto]) -> Result<HashMap<i64, Vec<EpisodeDto>>> {
        let mut out: HashMap<i64, Vec<EpisodeDto>> = series.iter().map(|s| (s.id, Vec::new())).collect();
        for chunk in series.chunks(500) {
            let ids: Vec<i64> = chunk.iter().map(|s| s.id).collect();
            let sql = format!("series_id IN ({})", placeholders(ids.len()));
            for e in repo::episodes_where(&self.svc.db, &sql, &ids).await? {
                if let Some(eps) = out.get_mut(&e.series_id) {
                    eps.push(e);
                }
            }
        }
        if out.values().flatten().any(|e| e.episode_file_id.is_some()) {
            let mut files = self.svc.episode_files_of_many(series).await?;
            for (series_id, eps) in &mut out {
                let files: HashMap<i64, EpisodeFileDto> = files
                    .remove(series_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| (f.file.id, f))
                    .collect();
                for e in eps.iter_mut() {
                    e.episode_file = e.episode_file_id.and_then(|id| files.get(&id).cloned());
                }
            }
        }
        Ok(out)
    }

    /// Episodes by id (any series), with files attached.
    pub async fn episodes_by_ids(&self, ids: &[i64]) -> Result<Vec<EpisodeDto>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut eps = repo::episodes_by_ids(&self.svc.db, ids).await?;
        let mut series_ids: Vec<i64> = eps.iter().map(|e| e.series_id).collect();
        series_ids.sort_unstable();
        series_ids.dedup();
        let all = repo::load(&self.svc.db, Some(&series_ids), self.svc.today()).await?;
        // Sorted once, each series' episodes are one run of the list: no regrouping per series.
        eps.sort_by_key(|e| (e.series_id, e.season_number, e.episode_number));
        for s in &all {
            let from = eps.partition_point(|e| e.series_id < s.id);
            let to = eps.partition_point(|e| e.series_id <= s.id);
            if from < to {
                self.attach_files(s, &mut eps[from..to]).await?;
            }
        }
        Ok(eps)
    }

    async fn attach_files(&self, s: &SeriesDto, eps: &mut [EpisodeDto]) -> Result<()> {
        if !eps.iter().any(|e| e.episode_file_id.is_some()) {
            return Ok(());
        }
        let files: HashMap<i64, EpisodeFileDto> = self
            .svc
            .episode_files_of(s)
            .await?
            .into_iter()
            .map(|f| (f.file.id, f))
            .collect();
        for e in eps {
            e.episode_file = e.episode_file_id.and_then(|id| files.get(&id).cloned());
        }
        Ok(())
    }
}
