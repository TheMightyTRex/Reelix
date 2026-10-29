//! Artist metadata refresh: artist details, new/removed release groups (filtered by the
//! metadata profile) and the tracklists of monitored albums.

use std::collections::{HashMap, HashSet};

use reelix_api_types::library::{ImagesDto, RatingsDto, RescanAfterRefresh};
use reelix_api_types::music::{ArtistDto, ArtistMonitor};
use reelix_db::now_ms;
use reelix_metadata::{ArtistMetadata, ReleaseGroupMetadata};

use crate::artists::artist_fields;
use crate::convert::{album_type, parse_date, secondary_type};
use crate::error::Result;
use crate::profiles::allows_group;
use crate::repo::albums::AlbumFields;
use crate::{MusicService, repo};

/// Artists refreshed by the scheduled task are skipped when refreshed within this window.
pub const STALE_AFTER_MS: i64 = 7 * 24 * 3600 * 1000;

pub(crate) fn album_fields(rg: &ReleaseGroupMetadata) -> AlbumFields {
    AlbumFields {
        musicbrainz_release_group_id: rg.id.clone(),
        title: rg.title.clone(),
        disambiguation: rg.disambiguation.clone().filter(|d| !d.is_empty()),
        album_type: album_type(rg.primary_type.as_deref()),
        secondary_types: rg
            .secondary_types
            .iter()
            .filter_map(|s| secondary_type(s))
            .collect(),
        release_date: parse_date(rg.first_release_date.as_deref()),
        genres: rg.genres.clone(),
        images: ImagesDto::default(),
        ratings: RatingsDto {
            vote_average: rg.rating.unwrap_or_default() * 2.0,
            vote_count: 0,
            popularity: 0.0,
        },
    }
}

impl MusicService {
    /// Inserts/updates the artist's release groups that pass the metadata profile and removes
    /// albums (without files) that no longer do. `initial` = the add-time monitor option.
    pub(crate) async fn sync_albums(
        &self,
        artist: &ArtistDto,
        meta: &ArtistMetadata,
        initial: Option<(ArtistMonitor, &[String])>,
    ) -> Result<()> {
        let profile = self.metadata_profile(artist.metadata_profile_id).await?;
        let today = self.today();
        let wanted: Vec<&ReleaseGroupMetadata> = meta
            .release_groups
            .iter()
            .filter(|rg| allows_group(&profile, rg))
            .collect();
        let existing: HashMap<String, (i64, bool, u64)> = repo::albums::of_artists(&self.db, &[artist.id])
            .await?
            .into_iter()
            .map(|a| {
                (
                    a.musicbrainz_release_group_id.clone(),
                    (a.id, a.monitored, a.statistics.size_on_disk),
                )
            })
            .collect();
        // Initial monitoring for Latest/First.
        let dated: Vec<(&str, chrono::NaiveDate)> = wanted
            .iter()
            .filter_map(|rg| parse_date(rg.first_release_date.as_deref()).map(|d| (rg.id.as_str(), d)))
            .filter(|(_, d)| *d <= today)
            .collect();
        let latest = dated.iter().max_by_key(|(_, d)| *d).map(|(id, _)| *id);
        let first = dated.iter().min_by_key(|(_, d)| *d).map(|(id, _)| *id);
        let mut seen = HashSet::new();
        for rg in &wanted {
            seen.insert(rg.id.clone());
            let fields = album_fields(rg);
            if let Some((id, _, _)) = existing.get(&rg.id) {
                repo::albums::update_fields(&self.db, *id, &fields).await?;
                continue;
            }
            let monitored = match initial {
                Some((mode, selected)) => match mode {
                    ArtistMonitor::All | ArtistMonitor::Missing => true,
                    ArtistMonitor::Future => fields.release_date.is_none_or(|d| d > today),
                    ArtistMonitor::Latest => latest == Some(rg.id.as_str()),
                    ArtistMonitor::First => first == Some(rg.id.as_str()),
                    ArtistMonitor::Selected => selected.iter().any(|s| s == &rg.id),
                    ArtistMonitor::Existing | ArtistMonitor::None => false,
                },
                None => self.monitor_new(artist, fields.release_date),
            };
            // Another artist may already own the group (collaborations): skip it.
            if repo::albums::by_mbid(&self.db, &rg.id).await?.is_some() {
                continue;
            }
            let id = repo::albums::insert(&self.db, artist.id, &fields, monitored, None).await?;
            // On add, tracklists are fetched by the queued refresh (MusicBrainz allows 1 req/s);
            // albums found by a refresh (a job) get theirs right away.
            if initial.is_none() {
                if monitored {
                    self.ensure_releases_quiet(id).await;
                }
                self.emit("album", "created", id);
            }
        }
        for (rg, (id, monitored, size)) in &existing {
            // Albums added explicitly (monitored) stay even when the profile excludes them.
            if !seen.contains(rg) && *size == 0 && !*monitored {
                repo::albums::delete(&self.db, *id).await?;
                self.emit("album", "deleted", *id);
            }
        }
        Ok(())
    }

    /// Refreshes one artist; `force_releases` re-fetches the tracklists of monitored albums.
    pub(crate) async fn refresh_artist(&self, id: i64, force_releases: bool) -> Result<()> {
        let artist = repo::artists::require(&self.db, id).await?;
        let meta = self.metadata.artist(&artist.musicbrainz_id).await?;
        repo::artists::update_fields(&self.db, id, &artist_fields(&meta)).await?;
        let artist = repo::artists::require(&self.db, id).await?;
        self.sync_albums(&artist, &meta, None).await?;
        for a in repo::albums::of_artists(&self.db, &[id]).await? {
            if a.monitored
                && let Err(e) = self.ensure_releases(a.id, force_releases).await
            {
                tracing::warn!(album_id = a.id, error = %e, "release refresh failed");
            }
        }
        self.emit("artist", "updated", id);
        Ok(())
    }

    /// Refreshes the given artists (all when `None` or empty; the scheduled run skips artists
    /// refreshed within 7 days); returns how many failed.
    pub async fn refresh_artists(&self, ids: Option<Vec<i64>>, manual: bool) -> Result<usize> {
        let ids = match ids.filter(|v| !v.is_empty()) {
            Some(v) => v,
            None if manual => repo::artists::load(&self.db, None)
                .await?
                .into_iter()
                .map(|a| a.id)
                .collect(),
            None => repo::artists::stale(&self.db, now_ms() - STALE_AFTER_MS).await?,
        };
        let rescan = match self.library.media_management().await?.rescan_after_refresh {
            RescanAfterRefresh::Always => true,
            RescanAfterRefresh::AfterManual => manual,
            RescanAfterRefresh::Never => false,
        };
        let mut failed = 0;
        for id in ids {
            match self.refresh_artist(id, manual).await {
                Ok(()) => {
                    if rescan && let Err(e) = self.scan_artists(&[id]).await {
                        tracing::warn!(artist_id = id, error = %e, "rescan after refresh failed");
                    }
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(artist_id = id, error = %e, "artist refresh failed");
                }
            }
        }
        Ok(failed)
    }
}
