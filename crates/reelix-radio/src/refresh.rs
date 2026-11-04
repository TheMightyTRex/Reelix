//! Refresh: read a show from its provider and merge it into the saved series and episodes.
//!
//! - Settled series are not read again ([`ShowFetch::skip_series`]).
//! - New episodes are monitored from the follow choice; saved episodes keep their state.
//! - Nothing is deleted when the source drops a series or episode: it gets
//!   `source_removed_at` (cleared when it comes back).
//! - Episodes (and series) without a number from the source are numbered in broadcast order.
//! - A failed read leaves the saved data as it was.

use std::collections::{HashMap, HashSet};

use reelix_api_types::radio::{RadioNumberSource, RadioSeriesKind};

use crate::error::{RadioError, Result};
use crate::provider::{SeriesData, ShowData, ShowFetch};
use crate::status::{is_settled, monitored_by_follow};
use crate::{RadioService, entities, repo};

/// Outcome of [`RadioService::refresh_shows`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefreshSummary {
    pub refreshed: u32,
    pub failed: u32,
    pub episodes_added: u32,
}

impl RadioService {
    /// Refreshes `ids` (`None` = every show). `feeds_only` limits it to shows of feed
    /// providers (`radio_feed_sync`); `skip_feeds` leaves them out (the 12-hour refresh, since
    /// feeds are read hourly anyway). One show failing does not stop the others.
    pub async fn refresh_shows(
        &self,
        ids: Option<Vec<i64>>,
        feeds_only: bool,
        skip_feeds: bool,
    ) -> Result<RefreshSummary> {
        let ids = match ids.filter(|i| !i.is_empty()) {
            Some(ids) => ids,
            None => repo::shows::ids(&self.db, None).await?,
        };
        let mut sum = RefreshSummary::default();
        for id in ids {
            let Some(show) = repo::shows::get(&self.db, id).await? else {
                continue;
            };
            let is_feed = self.providers.get(&show.provider).is_some_and(|p| p.is_feed());
            if (feeds_only && !is_feed) || (skip_feeds && is_feed) {
                continue;
            }
            match self.refresh_show(id).await {
                Ok(added) => {
                    sum.refreshed += 1;
                    sum.episodes_added += added;
                }
                Err(e) => {
                    sum.failed += 1;
                    tracing::warn!(show_id = id, title = %show.title, error = %e, "radio show refresh failed");
                }
            }
        }
        Ok(sum)
    }

    /// Reads one show from its provider and merges it. Returns the number of new episodes.
    pub async fn refresh_show(&self, id: i64) -> Result<u32> {
        let show = repo::shows::require(&self.db, id).await?;
        let provider = self.providers.get(&show.provider).ok_or_else(|| {
            RadioError::Validation(format!(
                "the source {} of {} is not available",
                show.provider, show.title
            ))
        })?;
        let settings = self.settings().await?;
        let series = repo::series::for_show(&self.db, id).await?;
        let fetch = ShowFetch {
            skip_series: series
                .iter()
                .filter(|s| s.settled_at.is_some())
                .map(|s| s.external_id.clone())
                .collect(),
            known_episodes: repo::episodes::known_details(&self.db, id)
                .await?
                .into_iter()
                .collect(),
            max_lookups: settings.max_lookups_per_refresh,
            quick: false,
        };
        let data = provider.show(&show.external_id, &fetch).await?;
        let added = self.merge(id, &data, &fetch.skip_series, false).await?;
        self.download_artwork(id).await;
        self.emit(entities::SHOW, "updated", id);
        Ok(added)
    }

    /// Saves a show's first read (when it is added).
    pub(crate) async fn apply_show_data(&self, id: i64, data: &ShowData, initial: bool) -> Result<u32> {
        self.merge(id, data, &HashSet::new(), initial).await
    }

    async fn merge(&self, id: i64, data: &ShowData, skipped: &HashSet<String>, initial: bool) -> Result<u32> {
        let show = repo::shows::require(&self.db, id).await?;
        let station_id = match &data.station {
            Some(st) => Some(repo::stations::upsert(&self.db, &show.provider, st).await?),
            None => None,
        };
        repo::shows::update_from_source(&self.db, id, data, station_id).await?;

        // Series.
        let mut series_ids: HashMap<String, i64> = HashMap::new();
        for s in &data.series {
            series_ids.insert(
                s.external_id.clone(),
                repo::series::upsert(&self.db, id, s).await?,
            );
        }
        for e in &data.episodes {
            if !series_ids.contains_key(&e.series_external_id) {
                let s = SeriesData {
                    external_id: e.series_external_id.clone(),
                    ..SeriesData::default()
                };
                series_ids.insert(
                    s.external_id.clone(),
                    repo::series::upsert(&self.db, id, &s).await?,
                );
            }
        }
        let saved_series = repo::series::for_show(&self.db, id).await?;
        if data.complete {
            let gone: Vec<i64> = saved_series
                .iter()
                .filter(|s| !series_ids.contains_key(&s.external_id) && !skipped.contains(&s.external_id))
                .map(|s| s.id)
                .collect();
            repo::series::mark_removed(&self.db, &gone).await?;
        }
        let skipped_ids: HashSet<i64> = saved_series
            .iter()
            .filter(|s| skipped.contains(&s.external_id))
            .map(|s| s.id)
            .collect();

        // Episodes.
        let saved: HashMap<String, (i64, i64)> = repo::episodes::for_show(&self.db, id)
            .await?
            .into_iter()
            .map(|e| (e.external_id, (e.id, e.series_id)))
            .collect();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut added = 0;
        for e in &data.episodes {
            if !seen.insert(e.external_id.as_str()) {
                continue; // feeds sometimes repeat an item
            }
            let series_id = series_ids[&e.series_external_id];
            match saved.get(&e.external_id) {
                Some((eid, _)) => repo::episodes::update_from_source(&self.db, *eid, series_id, e).await?,
                None => {
                    let monitored =
                        monitored_by_follow(show.follow, show.followed_at, e.first_broadcast, !initial);
                    let eid = repo::episodes::insert(&self.db, id, series_id, e, monitored).await?;
                    added += 1;
                    if !initial {
                        self.emit(entities::EPISODE, "created", eid);
                    }
                }
            }
        }
        if data.complete {
            let gone: Vec<i64> = saved
                .iter()
                .filter(|(ext, (_, sid))| !seen.contains(ext.as_str()) && !skipped_ids.contains(sid))
                .map(|(_, (eid, _))| *eid)
                .collect();
            repo::episodes::mark_removed(&self.db, &gone).await?;
        }

        self.number_in_broadcast_order(id).await?;
        self.settle_series(id).await?;
        self.recompute_statuses(id).await?;
        Ok(added)
    }

    /// Gives series and episodes that have no number from the source the lowest numbers not
    /// taken by source numbers, in broadcast order (undated last).
    async fn number_in_broadcast_order(&self, show_id: i64) -> Result<()> {
        let series = repo::series::for_show(&self.db, show_id).await?;
        let episodes = repo::episodes::for_show(&self.db, show_id).await?;
        let first_of = |sid: i64| {
            episodes
                .iter()
                .filter(|e| e.series_id == sid)
                .filter_map(|e| e.first_broadcast)
                .min()
        };

        let mut used: HashSet<u32> = series
            .iter()
            .filter(|s| s.kind == RadioSeriesKind::Series && s.number_source == RadioNumberSource::Source)
            .filter_map(|s| s.number)
            .collect();
        let mut todo: Vec<_> = series
            .iter()
            .filter(|s| {
                s.kind == RadioSeriesKind::Series && s.number_source == RadioNumberSource::BroadcastOrder
            })
            .map(|s| (first_of(s.id), s.id))
            .collect();
        todo.sort_by_key(|(first, sid)| (first.is_none(), *first, *sid));
        let mut next = 1;
        for (_, sid) in todo {
            while used.contains(&next) {
                next += 1;
            }
            repo::series::set_derived_number(&self.db, sid, next).await?;
            used.insert(next);
        }

        for s in &series {
            let eps: Vec<_> = episodes.iter().filter(|e| e.series_id == s.id).collect();
            let mut used: HashSet<u32> = eps
                .iter()
                .filter(|e| e.number_source == RadioNumberSource::Source)
                .filter_map(|e| e.number)
                .collect();
            let mut todo: Vec<_> = eps
                .iter()
                .filter(|e| e.number_source == RadioNumberSource::BroadcastOrder)
                .collect();
            todo.sort_by_key(|e| (e.first_broadcast.is_none(), e.first_broadcast, e.id));
            let mut next = 1;
            for e in todo {
                while used.contains(&next) {
                    next += 1;
                }
                repo::episodes::set_derived_number(&self.db, e.id, next).await?;
                used.insert(next);
            }
        }
        Ok(())
    }

    /// Marks complete, quiet series settled ([`is_settled`]).
    async fn settle_series(&self, show_id: i64) -> Result<()> {
        let now = self.now();
        let episodes = repo::episodes::for_show(&self.db, show_id).await?;
        for s in repo::series::for_show(&self.db, show_id).await? {
            if s.settled_at.is_some() {
                continue;
            }
            let last = episodes
                .iter()
                .filter(|e| e.series_id == s.id)
                .filter_map(|e| e.first_broadcast)
                .max();
            if is_settled(s.expected_episodes, s.episode_count, last, now) {
                repo::series::set_settled(&self.db, s.id, now.timestamp_millis()).await?;
            }
        }
        Ok(())
    }
}
