//! What the pipeline needs to search for albums: targets and release matching.

use std::collections::HashMap;

use chrono::NaiveDate;
use reelix_api_types::music::{AlbumDto, AlbumReleaseDto, ArtistDto, TrackDto, TrackFileDto};
use reelix_import::matching::similarity;
use reelix_parser::ParsedMusic;

use crate::convert::clean_name;
use crate::error::Result;
use crate::{MusicService, repo};

/// Which targets a search wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    Missing,
    Cutoff,
    MissingOrCutoff,
    Any,
}

/// One album to search for.
#[derive(Debug, Clone)]
pub struct AlbumTarget {
    pub artist: ArtistDto,
    pub album: AlbumDto,
    pub release: Option<AlbumReleaseDto>,
    pub tracks: Vec<TrackDto>,
    pub quality_profile_id: i64,
    pub existing: Vec<TrackFileDto>,
    pub tags: Vec<i64>,
    /// Sum of the track durations.
    pub runtime_minutes: Option<u32>,
}

/// Borrowed view of [`MusicService`] for the pipeline.
pub struct MusicSearchApi<'a> {
    pub(crate) svc: &'a MusicService,
}

impl MusicSearchApi<'_> {
    pub fn today(&self) -> NaiveDate {
        self.svc.today()
    }

    pub async fn target(&self, album_id: i64) -> Result<AlbumTarget> {
        let svc = self.svc;
        let album = svc.get_album(album_id).await?;
        let artist = repo::artists::require(&svc.db, album.artist_id).await?;
        let tracks = svc.tracks(album_id).await?;
        let existing = svc.track_files(album_id).await?;
        let total_ms: u64 = tracks.iter().filter_map(|t| t.duration_ms).sum();
        Ok(AlbumTarget {
            release: album.releases.iter().find(|r| r.selected).cloned(),
            quality_profile_id: artist.quality_profile_id,
            tags: artist.tags.clone(),
            runtime_minutes: (total_ms > 0).then(|| total_ms.div_ceil(60_000) as u32),
            artist,
            album,
            tracks,
            existing,
        })
    }

    async fn filter(&self, albums: Vec<AlbumDto>, wanted: Wanted) -> Result<Vec<AlbumTarget>> {
        let below = match wanted {
            Wanted::Cutoff | Wanted::MissingOrCutoff => self.svc.below_cutoff(&albums).await?,
            _ => HashMap::new(),
        };
        let mut out = Vec::new();
        for a in albums {
            let missing = MusicService::is_missing(&a);
            let cutoff = a.statistics.size_on_disk > 0 && below.get(&a.id).copied().unwrap_or(false);
            let keep = match wanted {
                Wanted::Missing => missing,
                Wanted::Cutoff => cutoff,
                Wanted::MissingOrCutoff => missing || cutoff,
                Wanted::Any => true,
            };
            if keep {
                match self.target(a.id).await {
                    Ok(t) => out.push(t),
                    Err(e) => tracing::warn!(album_id = a.id, error = %e, "skipping album target"),
                }
            }
        }
        Ok(out)
    }

    /// Monitored album targets (empty `album_ids` = whole library), filtered by `wanted`.
    pub async fn targets(&self, album_ids: &[i64], wanted: Wanted) -> Result<Vec<AlbumTarget>> {
        let albums = if album_ids.is_empty() {
            repo::albums::wanted_candidates(&self.svc.db, self.today()).await?
        } else {
            repo::albums::load(&self.svc.db, Some(album_ids)).await?
        };
        self.filter(albums, wanted).await
    }

    pub async fn targets_for_artists(&self, artist_ids: &[i64], wanted: Wanted) -> Result<Vec<AlbumTarget>> {
        let today = self.today();
        let monitored: Vec<i64> = repo::artists::load(&self.svc.db, Some(artist_ids))
            .await?
            .into_iter()
            .filter(|a| a.monitored)
            .map(|a| a.id)
            .collect();
        let albums = repo::albums::of_artists(&self.svc.db, &monitored)
            .await?
            .into_iter()
            .filter(|a| a.monitored && a.release_date.is_none_or(|d| d <= today))
            .collect();
        self.filter(albums, wanted).await
    }

    /// RSS / unknown-download mapping to an album id (artist + album title; discographies → `None`).
    pub async fn match_release(&self, parsed: &ParsedMusic) -> Result<Option<i64>> {
        if parsed.is_discography {
            return Ok(None);
        }
        let (Some(artist), Some(album)) = (parsed.artist.as_deref(), parsed.album.as_deref()) else {
            return Ok(None);
        };
        let key = clean_name(artist);
        if key.is_empty() {
            return Ok(None);
        }
        let artists = repo::artists::clean_names(&self.svc.db).await?;
        let Some(artist_id) = artists
            .iter()
            .find(|(_, c)| *c == key)
            .or_else(|| {
                artists
                    .iter()
                    .filter(|(_, c)| similarity(c, &key) >= 0.9)
                    .max_by(|a, b| similarity(&a.1, &key).total_cmp(&similarity(&b.1, &key)))
            })
            .map(|(id, _)| *id)
        else {
            return Ok(None);
        };
        let wanted = clean_name(album);
        let titles = repo::albums::clean_titles(&self.svc.db, artist_id).await?;
        if let Some((id, _)) = titles.iter().find(|(_, c)| *c == wanted) {
            return Ok(Some(*id));
        }
        // "Album (Deluxe Edition)" etc.: strip the edition and compare again.
        let stripped = match &parsed.edition {
            Some(e) => clean_name(&album.replace(e.as_str(), "")),
            None => wanted.clone(),
        };
        Ok(titles
            .iter()
            .map(|(id, c)| (*id, similarity(c, &stripped).max(similarity(c, &wanted))))
            .filter(|(_, s)| *s >= 0.85)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id))
    }
}
