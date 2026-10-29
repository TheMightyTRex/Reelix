//! Releases (editions) of an album: fetching their tracklists, picking the wanted release and
//! moving file links when the selected release changes.

use std::collections::HashMap;

use reelix_api_types::library::ImagesDto;
use reelix_api_types::music::{AlbumDto, ChooseReleaseRequest, MusicSettingsDto, ReleaseStatus};
use reelix_metadata::ReleaseMetadata;

use crate::convert::{credit_name, parse_date, release_status};
use crate::error::{MusicError, Result};
use crate::matching::CandidateRelease;
use crate::profiles::allows_status;
use crate::repo::releases::ReleaseRow;
use crate::repo::tracks::NewTrack;
use crate::{MusicService, repo};

/// `CD`, `2×CD`, `CD + DVD`, from the media formats of a release.
fn format_summary(r: &ReleaseMetadata) -> Option<String> {
    let mut parts: Vec<(String, u32)> = Vec::new();
    for m in &r.media {
        let f = m.format.clone().unwrap_or_else(|| "Unknown".into());
        match parts.iter_mut().find(|(name, _)| *name == f) {
            Some(p) => p.1 += 1,
            None => parts.push((f, 1)),
        }
    }
    if parts.is_empty() || parts.iter().all(|(f, _)| f == "Unknown") {
        return None;
    }
    Some(
        parts
            .into_iter()
            .map(|(f, n)| if n > 1 { format!("{n}×{f}") } else { f })
            .collect::<Vec<_>>()
            .join(" + "),
    )
}

/// Higher is better: official, preferred country/format, the most common track count, CD/digital.
fn release_preference(r: &ReleaseRow, common_tracks: u32, s: &MusicSettingsDto) -> i32 {
    let mut score = 0;
    if r.status.unwrap_or(ReleaseStatus::Official) == ReleaseStatus::Official {
        score += 8;
    }
    if r.country.as_deref().is_some_and(|c| {
        s.preferred_release_countries
            .iter()
            .any(|p| p.eq_ignore_ascii_case(c))
    }) {
        score += 4;
    }
    let fmt = r.format.clone().unwrap_or_default().to_ascii_lowercase();
    if s.preferred_formats
        .iter()
        .any(|p| !p.is_empty() && fmt.contains(&p.to_ascii_lowercase()))
    {
        score += 4;
    }
    if r.track_count == common_tracks {
        score += 2;
    }
    if fmt.contains("cd") || fmt.contains("digital") {
        score += 1;
    }
    score
}

/// Picks the default release of an album.
pub(crate) fn pick_release(releases: &[ReleaseRow], s: &MusicSettingsDto) -> Option<i64> {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    for r in releases.iter().filter(|r| r.track_count > 0) {
        *counts.entry(r.track_count).or_default() += 1;
    }
    let common = counts
        .into_iter()
        .max_by_key(|(tracks, n)| (*n, std::cmp::Reverse(*tracks)))
        .map_or(0, |(t, _)| t);
    releases
        .iter()
        .filter(|r| r.track_count > 0)
        .max_by(|a, b| {
            release_preference(a, common, s)
                .cmp(&release_preference(b, common, s))
                // earlier releases first on ties (max_by keeps the last max → compare reversed)
                .then_with(|| b.release_date.cmp(&a.release_date))
                .then_with(|| b.id.cmp(&a.id))
        })
        .map(|r| r.id)
}

impl MusicService {
    /// Fetches the album's releases and tracklists unless they were fetched before (`force`
    /// re-fetches), picks a release when none is selected and fills a missing cover.
    pub(crate) async fn ensure_releases(&self, album_id: i64, force: bool) -> Result<()> {
        if !force && repo::albums::releases_synced(&self.db, album_id).await? {
            return Ok(());
        }
        let album = repo::albums::require(&self.db, album_id).await?;
        let artist = repo::artists::require(&self.db, album.artist_id).await?;
        let profile = self.metadata_profile(artist.metadata_profile_id).await.ok();
        let settings = self.settings().await?;
        let fetched = self
            .metadata
            .releases(&album.musicbrainz_release_group_id)
            .await?;
        let allowed: Vec<&ReleaseMetadata> = fetched
            .iter()
            .filter(|r| {
                profile
                    .as_ref()
                    .is_none_or(|p| allows_status(p, release_status(r.status.as_deref())))
            })
            .collect();
        // A group whose releases all have disallowed statuses keeps them rather than none.
        let chosen: Vec<&ReleaseMetadata> = if allowed.is_empty() {
            fetched.iter().collect()
        } else {
            allowed
        };
        let mut keep = Vec::new();
        for r in chosen {
            let row = ReleaseRow {
                id: 0,
                album_id,
                musicbrainz_release_id: r.id.clone(),
                title: r.title.clone(),
                status: release_status(r.status.as_deref()),
                country: r.country.clone(),
                label: r.label.clone(),
                catalog_number: r.catalog_number.clone(),
                barcode: r.barcode.clone(),
                format: format_summary(r),
                disc_count: r.media.len().max(1) as u32,
                track_count: r.media.iter().map(|m| m.tracks.len() as u32).sum(),
                release_date: parse_date(r.date.as_deref()),
                disambiguation: r.disambiguation.clone().filter(|d| !d.is_empty()),
            };
            let id = repo::releases::upsert(&self.db, &row).await?;
            let mut absolute = 0;
            let mut tracks = Vec::new();
            for (mi, m) in r.media.iter().enumerate() {
                let disc = if m.position > 0 { m.position } else { mi as u32 + 1 };
                for (ti, t) in m.tracks.iter().enumerate() {
                    absolute += 1;
                    tracks.push(NewTrack {
                        musicbrainz_track_id: Some(t.id.clone()).filter(|s| !s.is_empty()),
                        musicbrainz_recording_id: Some(t.recording_id.clone()).filter(|s| !s.is_empty()),
                        disc_number: disc,
                        track_number: if t.position > 0 { t.position } else { ti as u32 + 1 },
                        absolute_number: absolute,
                        number: Some(t.number.clone()).filter(|s| !s.is_empty()),
                        title: t.title.clone(),
                        duration_ms: t.length_ms,
                        artist_credit: credit_name(&t.artist_credit),
                    });
                }
            }
            repo::tracks::replace(&self.db, album_id, id, &tracks).await?;
            keep.push(id);
        }
        if !keep.is_empty() {
            repo::releases::delete_others(&self.db, album_id, &keep).await?;
        }
        let rows = repo::releases::of_album(&self.db, album_id).await?;
        let current = album
            .selected_release_id
            .filter(|id| rows.iter().any(|r| r.id == *id));
        if current.is_none()
            && let Some(pick) = pick_release(&rows, &settings)
        {
            repo::albums::set_selected_release(&self.db, album_id, Some(pick)).await?;
        }
        if album.images.poster.is_none() {
            match self.metadata.cover_art(&album.musicbrainz_release_group_id).await {
                Ok(Some(c)) => {
                    let images = ImagesDto {
                        poster: c.thumb_500.or(c.front.clone()),
                        backdrop: None,
                        logo: None,
                    };
                    if images.poster.is_some() {
                        repo::albums::set_images(&self.db, album_id, &images).await?;
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::debug!(album_id, error = %e, "cover art lookup failed"),
            }
        }
        repo::albums::mark_synced(&self.db, album_id).await?;
        Ok(())
    }

    /// [`ensure_releases`](Self::ensure_releases), logging instead of failing.
    pub(crate) async fn ensure_releases_quiet(&self, album_id: i64) {
        if let Err(e) = self.ensure_releases(album_id, false).await {
            tracing::warn!(album_id, error = %e, "could not fetch album releases");
        }
    }

    /// Every release of the album with its tracklist.
    pub(crate) async fn candidates(&self, album_id: i64) -> Result<Vec<CandidateRelease>> {
        let releases = repo::releases::of_album(&self.db, album_id).await?;
        let mut tracks = repo::tracks::of_album(&self.db, album_id).await?;
        let mut by_release: HashMap<i64, Vec<_>> = HashMap::new();
        for t in tracks.drain(..) {
            by_release.entry(t.release_id).or_default().push(t);
        }
        Ok(releases
            .into_iter()
            .map(|r| CandidateRelease {
                release_id: r.id,
                musicbrainz_release_id: r.musicbrainz_release_id,
                title: r.title,
                country: r.country,
                format: r.format,
                tracks: by_release.remove(&r.id).unwrap_or_default(),
            })
            .collect())
    }

    /// Moves the file links from release `from` to release `to` by disc/track number.
    pub(crate) async fn relink(&self, from: Option<i64>, to: i64) -> Result<()> {
        let Some(from) = from.filter(|f| *f != to) else {
            return Ok(());
        };
        let old = repo::tracks::of_release(&self.db, from).await?;
        let new = repo::tracks::of_release(&self.db, to).await?;
        for t in old.iter().filter(|t| t.track_file_id.is_some()) {
            if let Some(n) = new
                .iter()
                .find(|n| n.disc_number == t.disc_number && n.track_number == t.track_number)
                && n.track_file_id.is_none()
            {
                repo::tracks::link_file(&self.db, &[n.id], t.track_file_id).await?;
            }
        }
        let ids: Vec<i64> = old.iter().map(|t| t.id).collect();
        repo::tracks::link_file(&self.db, &ids, None).await?;
        Ok(())
    }

    /// Makes `release_id` the album's selected release (moving file links).
    pub(crate) async fn select_release(&self, album: &AlbumDto, release_id: i64) -> Result<()> {
        if album.selected_release_id == Some(release_id) {
            return Ok(());
        }
        self.relink(album.selected_release_id, release_id).await?;
        repo::albums::set_selected_release(&self.db, album.id, Some(release_id)).await?;
        Ok(())
    }

    pub async fn choose_release(&self, album_id: i64, req: ChooseReleaseRequest) -> Result<AlbumDto> {
        let album = repo::albums::require(&self.db, album_id).await?;
        let release = repo::releases::get(&self.db, req.release_id)
            .await?
            .filter(|r| r.album_id == album_id)
            .ok_or_else(|| {
                MusicError::Validation(format!(
                    "release {} does not belong to album {album_id}",
                    req.release_id
                ))
            })?;
        self.select_release(&album, release.id).await?;
        self.emit("album", "updated", album_id);
        self.get_album(album_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, country: &str, format: &str, tracks: u32, date: &str) -> ReleaseRow {
        ReleaseRow {
            id,
            album_id: 1,
            musicbrainz_release_id: format!("r{id}"),
            title: "X".into(),
            status: Some(ReleaseStatus::Official),
            country: Some(country.into()),
            label: None,
            catalog_number: None,
            barcode: None,
            format: Some(format.into()),
            disc_count: 1,
            track_count: tracks,
            release_date: parse_date(Some(date)),
            disambiguation: None,
        }
    }

    #[test]
    fn picks_common_preferred_release() {
        let rs = vec![
            row(1, "JP", "CD", 14, "1997-05-21"),
            row(2, "GB", "CD", 12, "1997-06-16"),
            row(3, "US", "CD", 12, "1997-07-01"),
            row(4, "XW", "Digital Media", 12, "2009-01-01"),
        ];
        let mut s = MusicSettingsDto::default();
        assert_eq!(pick_release(&rs, &s), Some(2));
        s.preferred_release_countries = vec!["US".into()];
        assert_eq!(pick_release(&rs, &s), Some(3));
        s.preferred_release_countries.clear();
        s.preferred_formats = vec!["Digital".into()];
        assert_eq!(pick_release(&rs, &s), Some(4));
        assert_eq!(pick_release(&[], &s), None);
    }
}
