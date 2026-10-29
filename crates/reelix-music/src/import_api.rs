//! Download import for the pipeline: one finished download → an album's track files.
//!
//! 1. Read every audio file of the download (properties + tags).
//! 2. Match the files against the album's releases ([`crate::matching`]); ambiguous matches
//!    return [`AlbumImportOutcome::NeedsRelease`] for the user to choose.
//! 3. Check the quality profile (allowed qualities, upgrade over existing files).
//! 4. Stage every file into `<album folder>.reelix-partial/` next to the destination (same
//!    filesystem), writing tags and the cover into the staged copies. When tags are written,
//!    hardlinks become copies so seeding files are never modified (design D14); `Move` is
//!    staged as a copy and the sources are removed after the commit.
//! 5. Commit: replaced files are moved aside, staged files renamed into place, then the
//!    database is updated; on failure everything is rolled back (all tracks or none).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use reelix_api_types::music::{AlbumDto, ArtistDto, ReleaseMatchDto, TrackDto};
use reelix_import::content::ContentDownload;
use reelix_import::files::{PermissionSettings, TransferMode, TransferOptions, transfer_with};
use reelix_parser::{QualityModel, parse_music};

use crate::error::{MusicError, Result};
use crate::matching::{ACCEPT_DISTANCE, MatchDecision, Preferences, decide};
use crate::naming::{TrackFacts, codec_name, track_path};
use crate::repo::releases::ReleaseRow;
use crate::scan::{file_row, read_dir_audio};
use crate::tags::{AudioInfo, Cover, FileTags};
use crate::{MusicService, repo};

/// One imported audio file and the tracks it covers.
#[derive(Debug, Clone)]
pub struct ImportedTrackFile {
    pub track_ids: Vec<i64>,
    pub file_id: i64,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub size: u64,
    pub quality: QualityModel,
}

#[derive(Debug, Clone)]
pub struct ImportedAlbum {
    pub album_id: i64,
    pub artist_id: i64,
    pub release_id: i64,
    pub files: Vec<ImportedTrackFile>,
    pub is_upgrade: bool,
    /// Files replaced by this import (moved to the recycle bin or deleted).
    pub replaced: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum AlbumImportOutcome {
    Imported(ImportedAlbum),
    /// Several releases match about equally well: the user must choose one
    /// (`PUT /albums/{id}/release`), then retry.
    NeedsRelease {
        album_id: i64,
        candidates: Vec<ReleaseMatchDto>,
    },
    Rejected(Vec<String>),
}

/// Borrowed view of [`MusicService`] for the pipeline.
pub struct MusicImportApi<'a> {
    pub(crate) svc: &'a MusicService,
}

/// One file to bring in.
struct Planned {
    info: AudioInfo,
    tracks: Vec<TrackDto>,
    /// Relative to the artist folder.
    rel: PathBuf,
    tags: FileTags,
}

const MAX_CANDIDATES: usize = 10;
const MAX_COVER_BYTES: usize = 15 * 1024 * 1024;

fn reject(msg: impl Into<String>) -> Result<AlbumImportOutcome> {
    Ok(AlbumImportOutcome::Rejected(vec![msg.into()]))
}

impl MusicImportApi<'_> {
    /// Imports one finished download for `album_id` (all files or none).
    pub async fn import_download(&self, d: &ContentDownload, album_id: i64) -> Result<AlbumImportOutcome> {
        let svc = self.svc;
        let album = repo::albums::require(&svc.db, album_id).await?;
        let artist = repo::artists::require(&svc.db, album.artist_id).await?;
        let settings = svc.settings().await?;
        let infos = read_dir_audio(d.source.clone()).await?;
        if infos.is_empty() {
            return reject(format!("no audio files found in {}", d.source.display()));
        }
        if let Err(e) = svc.ensure_releases(album_id, false).await {
            if repo::releases::of_album(&svc.db, album_id).await?.is_empty() {
                return Err(e);
            }
            tracing::warn!(album_id, error = %e, "using stored releases");
        }
        let album = repo::albums::require(&svc.db, album_id).await?;
        let all = svc.candidates(album_id).await?;
        if all.iter().all(|c| c.tracks.is_empty()) {
            return reject(format!("{} has no known tracklist", album.title));
        }
        let prefs = Preferences {
            countries: settings.preferred_release_countries.clone(),
            formats: settings.preferred_formats.clone(),
            selected: album.selected_release_id,
        };
        let restricted = !album.any_release_ok && album.selected_release_id.is_some();
        let candidates: Vec<_> = if restricted {
            all.iter()
                .filter(|c| Some(c.release_id) == album.selected_release_id)
                .cloned()
                .collect()
        } else {
            all.clone()
        };
        let score = match decide(&infos, &candidates, &prefs, ACCEPT_DISTANCE, true) {
            MatchDecision::Matched(s) => s,
            MatchDecision::Ambiguous(scores) => {
                return Ok(AlbumImportOutcome::NeedsRelease {
                    album_id,
                    candidates: scores.iter().take(MAX_CANDIDATES).map(|s| s.to_dto()).collect(),
                });
            }
            MatchDecision::NoMatch(scores) => {
                if restricted
                    && let MatchDecision::Matched(_) | MatchDecision::Ambiguous(_) =
                        decide(&infos, &all, &prefs, ACCEPT_DISTANCE, false)
                {
                    let mut all_scores: Vec<_> = all
                        .iter()
                        .map(|c| crate::matching::score_release(&infos, c, &prefs))
                        .collect();
                    all_scores.sort_by(|a, b| a.distance.total_cmp(&b.distance));
                    return Ok(AlbumImportOutcome::NeedsRelease {
                        album_id,
                        candidates: all_scores
                            .iter()
                            .take(MAX_CANDIDATES)
                            .map(|s| s.to_dto())
                            .collect(),
                    });
                }
                let best = scores.first().map_or(1.0, |s| s.distance);
                return reject(format!(
                    "the downloaded files do not match any release of {} (best distance {best:.2})",
                    album.title
                ));
            }
        };
        if score.mapping.is_empty() || u64::from(score.missing_tracks) * 2 > u64::from(score.track_count) {
            return reject(format!(
                "only {} of {} tracks of {} are in the download",
                score.track_count.saturating_sub(score.missing_tracks),
                score.track_count,
                album.title
            ));
        }
        // Quality profile.
        let profile = svc.quality_profile(artist.quality_profile_id).await?;
        let existing = repo::files::of_albums(&svc.db, &[album_id]).await?;
        let mut rejections = Vec::new();
        let mut new_rank: Option<usize> = None;
        if let Some(p) = &profile {
            for (fi, _) in &score.mapping {
                let q = infos[*fi].quality();
                if !p.is_allowed(q) {
                    rejections.push(format!(
                        "{} is {} which the profile {:?} does not allow",
                        infos[*fi].path.display(),
                        q.name(),
                        p.name
                    ));
                }
                let r = p.rank(q).unwrap_or(0);
                new_rank = Some(new_rank.map_or(r, |n| n.min(r)));
            }
        }
        if !rejections.is_empty() {
            return Ok(AlbumImportOutcome::Rejected(rejections));
        }
        let release_tracks = repo::tracks::of_release(&svc.db, score.release_id).await?;
        let linked: HashSet<i64> = release_tracks.iter().filter_map(|t| t.track_file_id).collect();
        let covers_all =
            !release_tracks.is_empty() && release_tracks.iter().all(|t| t.track_file_id.is_some());
        if let (Some(p), Some(new_rank)) = (&profile, new_rank)
            && !existing.is_empty()
        {
            let old_rank = existing
                .iter()
                .map(|f| p.rank(f.quality.quality).unwrap_or(0))
                .min()
                .unwrap_or(0);
            if new_rank < old_rank {
                return reject(format!(
                    "the existing files of {} are of better quality",
                    album.title
                ));
            }
            if covers_all && (new_rank == old_rank || !p.upgrade_allowed) {
                return reject(format!("not an upgrade for {}", album.title));
            }
            if covers_all
                && existing
                    .iter()
                    .all(|f| p.cutoff_met(f.quality.quality, f.custom_format_score))
            {
                return reject(format!("{} already meets the profile cutoff", album.title));
            }
        }
        let is_upgrade = !existing.is_empty();
        let release = repo::releases::get(&svc.db, score.release_id)
            .await?
            .ok_or_else(|| MusicError::Internal("matched release vanished".into()))?;
        let by_id: HashMap<i64, &TrackDto> = release_tracks.iter().map(|t| (t.id, t)).collect();
        let cfg = svc.naming_config().await?;
        let disc_count = release
            .disc_count
            .max(release_tracks.iter().map(|t| t.disc_number).max().unwrap_or(1));
        let per_disc: HashMap<u32, u32> = release_tracks.iter().fold(HashMap::new(), |mut m, t| {
            *m.entry(t.disc_number).or_default() += 1;
            m
        });
        let parsed = d.release_title.as_deref().map(parse_music);
        let mut plan: Vec<Planned> = Vec::new();
        let mut used = HashSet::new();
        for (fi, track_id) in &score.mapping {
            let Some(track) = by_id.get(track_id) else {
                continue;
            };
            let info = infos[*fi].clone();
            let facts = facts(&artist, &album, Some(&release), track, &info);
            let stem = info.path.file_stem().map(|s| s.to_string_lossy().into_owned());
            let rel = track_path(
                &settings.naming,
                &facts,
                disc_count,
                &info.ext,
                stem.as_deref(),
                &cfg,
            );
            if !used.insert(rel.clone()) {
                return reject(format!("two files would be named {}", rel.display()));
            }
            let tags = tags_for(
                &artist,
                &album,
                &release,
                track,
                per_disc.get(&track.disc_number).copied(),
                disc_count,
            );
            plan.push(Planned {
                info,
                tracks: vec![(*track).clone()],
                rel,
                tags,
            });
        }
        let base = PathBuf::from(&artist.path);
        let guard = svc.path_guard().await?;
        for p in &plan {
            guard.check(&base.join(&p.rel))?;
        }
        let album_dir = plan
            .first()
            .and_then(|p| p.rel.components().next())
            .map(|c| base.join(c.as_os_str()))
            .unwrap_or_else(|| base.clone());
        let write_tags = settings.write_tags;
        let cover = if settings.embed_cover || settings.save_cover_file {
            svc.fetch_cover(&album).await
        } else {
            None
        };
        let embed = (write_tags && settings.embed_cover)
            .then(|| cover.clone())
            .flatten();
        let modifies = write_tags;
        let mode = match d.mode {
            TransferMode::Hardlink | TransferMode::HardlinkOrCopy if modifies => TransferMode::Copy,
            TransferMode::Move => TransferMode::Copy,
            m => m,
        };
        // Files replaced: existing files linked to the matched tracks, or at a target path.
        let targets: HashSet<String> = plan
            .iter()
            .map(|p| p.rel.to_string_lossy().into_owned())
            .collect();
        let mapped_tracks: HashSet<i64> = plan.iter().flat_map(|p| p.tracks.iter().map(|t| t.id)).collect();
        let replaced_rows: Vec<_> = existing
            .iter()
            .filter(|f| {
                targets.contains(&f.relative_path)
                    || release_tracks
                        .iter()
                        .any(|t| t.track_file_id == Some(f.id) && mapped_tracks.contains(&t.id))
                    || (!linked.contains(&f.id) && album.selected_release_id != Some(score.release_id))
            })
            .cloned()
            .collect();
        let perms = svc.permissions().await?;
        let stage = album_dir.with_file_name(format!(
            "{}.reelix-partial",
            album_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        let jobs: Vec<(PathBuf, PathBuf, FileTags)> = plan
            .iter()
            .map(|p| (p.info.path.clone(), p.rel.clone(), p.tags.clone()))
            .collect();
        let replaced_paths: Vec<PathBuf> = replaced_rows
            .iter()
            .map(|f| base.join(&f.relative_path))
            .collect();
        let (base2, stage2, replaced2) = (base.clone(), stage.clone(), replaced_paths.clone());
        let committed = tokio::task::spawn_blocking(move || {
            stage_and_commit(
                &base2,
                &stage2,
                &jobs,
                mode,
                write_tags,
                embed.as_ref(),
                perms,
                &replaced2,
            )
        })
        .await
        .map_err(|e| MusicError::Internal(e.to_string()))??;
        // Database (files are in place now).
        let result: Result<Vec<ImportedTrackFile>> = async {
            for f in &replaced_rows {
                repo::files::delete(&svc.db, f.id).await?;
            }
            svc.select_release(&album, score.release_id).await?;
            let mut out = Vec::new();
            for p in &plan {
                let rel = p.rel.to_string_lossy().into_owned();
                let dest = base.join(&p.rel);
                let mut info = p.info.clone();
                info.size = std::fs::metadata(&dest).map_or(info.size, |m| m.len());
                info.tags = if write_tags { p.tags.clone() } else { info.tags };
                let mut row = file_row(
                    album_id,
                    &rel,
                    &info,
                    d.release_title.clone(),
                    Some(p.info.path.to_string_lossy().into_owned()),
                );
                row.release_group = parsed.as_ref().and_then(|m| m.release_group.clone());
                let file_id = repo::files::upsert(&svc.db, &row).await?;
                let ids: Vec<i64> = p.tracks.iter().map(|t| t.id).collect();
                repo::tracks::link_file(&svc.db, &ids, Some(file_id)).await?;
                out.push(ImportedTrackFile {
                    track_ids: ids,
                    file_id,
                    source: p.info.path.clone(),
                    destination: dest,
                    size: info.size,
                    quality: row.quality,
                });
            }
            Ok(out)
        }
        .await;
        let files = match result {
            Ok(f) => f,
            Err(e) => {
                // Put the old files back so disk and database agree again.
                let c = committed.clone();
                let _ = tokio::task::spawn_blocking(move || c.rollback()).await;
                return Err(e);
            }
        };
        // Old files: recycle bin / delete.
        for (backup, _) in &committed.backups {
            if let Err(e) = svc.remove_path(backup).await {
                tracing::warn!(path = %backup.display(), error = %e, "could not remove replaced file");
            }
        }
        let _ = std::fs::remove_dir_all(&stage);
        if settings.save_cover_file
            && let Some(c) = &cover
        {
            let dest = album_dir.join(c.file_name());
            if let Err(e) = std::fs::write(&dest, &c.data) {
                tracing::warn!(path = %dest.display(), error = %e, "could not save cover");
            }
        }
        if d.mode == TransferMode::Move {
            for p in &plan {
                if let Err(e) = std::fs::remove_file(&p.info.path) {
                    tracing::warn!(path = %p.info.path.display(), error = %e, "could not remove source");
                }
            }
        }
        svc.emit("album", "updated", album_id);
        svc.emit("artist", "updated", artist.id);
        Ok(AlbumImportOutcome::Imported(ImportedAlbum {
            album_id,
            artist_id: artist.id,
            release_id: score.release_id,
            files,
            is_upgrade,
            replaced: replaced_paths,
        }))
    }

    /// Folder of the album's artist (for media-server refresh paths).
    pub async fn destination_root(&self, album_id: i64) -> Result<PathBuf> {
        let album = repo::albums::require(&self.svc.db, album_id).await?;
        let artist = repo::artists::require(&self.svc.db, album.artist_id).await?;
        Ok(PathBuf::from(artist.path))
    }
}

/// Naming facts of one track.
pub(crate) fn facts(
    artist: &ArtistDto,
    album: &AlbumDto,
    release: Option<&ReleaseRow>,
    track: &TrackDto,
    info: &AudioInfo,
) -> TrackFacts {
    let quality = info.quality();
    TrackFacts {
        artist_name: artist.name.clone(),
        artist_sort_name: artist.sort_name.clone(),
        album_title: album.title.clone(),
        album_type: album.album_type,
        album_disambiguation: album.disambiguation.clone(),
        release_year: album
            .release_date
            .or_else(|| release.and_then(|r| r.release_date))
            .map(|d| chrono::Datelike::year(&d)),
        medium: track.disc_number,
        medium_name: None,
        track: track.track_number,
        track_title: track.title.clone(),
        track_artist: track.artist_credit.clone(),
        quality: Some(quality),
        audio_codec: codec_name(&info.ext, quality),
        audio_bitrate_kbps: info.bitrate_kbps,
    }
}

fn tags_for(
    artist: &ArtistDto,
    album: &AlbumDto,
    release: &ReleaseRow,
    track: &TrackDto,
    disc_tracks: Option<u32>,
    disc_count: u32,
) -> FileTags {
    let date = release.release_date.or(album.release_date);
    FileTags {
        title: Some(track.title.clone()),
        artist: Some(track.artist_credit.clone().unwrap_or_else(|| artist.name.clone())),
        album: Some(album.title.clone()),
        album_artist: Some(artist.name.clone()),
        track: Some(track.track_number),
        track_total: disc_tracks,
        disc: Some(track.disc_number),
        disc_total: Some(disc_count),
        year: date.map(|d| chrono::Datelike::year(&d).max(0) as u32),
        date: date.map(|d| d.format("%Y-%m-%d").to_string()),
        musicbrainz_track_id: track.musicbrainz_track_id.clone(),
        musicbrainz_recording_id: track.musicbrainz_recording_id.clone(),
        musicbrainz_release_id: Some(release.musicbrainz_release_id.clone()),
        musicbrainz_release_group_id: Some(album.musicbrainz_release_group_id.clone()),
        musicbrainz_artist_id: Some(artist.musicbrainz_id.clone()),
    }
}

/// What a commit did, for rollback and cleanup.
#[derive(Debug, Clone, Default)]
struct Committed {
    /// `(backup location, original location)` of replaced files.
    backups: Vec<(PathBuf, PathBuf)>,
    /// New files now in the library.
    placed: Vec<PathBuf>,
}

impl Committed {
    fn rollback(&self) {
        for p in &self.placed {
            let _ = std::fs::remove_file(p);
        }
        for (backup, original) in &self.backups {
            let _ = std::fs::rename(backup, original);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_and_commit(
    base: &Path,
    stage: &Path,
    jobs: &[(PathBuf, PathBuf, FileTags)],
    mode: TransferMode,
    write_tags: bool,
    cover: Option<&Cover>,
    perms: PermissionSettings,
    replaced: &[PathBuf],
) -> Result<Committed> {
    let cleanup = |e: MusicError| {
        let _ = std::fs::remove_dir_all(stage);
        Err(e)
    };
    if stage.exists() {
        let _ = std::fs::remove_dir_all(stage);
    }
    if let Err(e) = std::fs::create_dir_all(stage) {
        return cleanup(e.into());
    }
    let opts = TransferOptions {
        overwrite: true,
        permissions: perms,
        preserve_mtime: true,
    };
    // 1. Stage (and tag) every file.
    for (src, rel, tags) in jobs {
        let staged = stage.join(rel);
        if let Err(e) = transfer_with(src, &staged, mode, &opts) {
            return cleanup(e.into());
        }
        if write_tags && let Err(e) = crate::tags::write(&staged, tags, cover) {
            return cleanup(e);
        }
    }
    // 2. Move replaced files aside.
    let mut done = Committed::default();
    let aside = stage.join(".replaced");
    for (i, original) in replaced.iter().enumerate() {
        if !original.is_file() {
            continue;
        }
        let name = original
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let backup = aside.join(format!("{i}-{name}"));
        let moved = std::fs::create_dir_all(&aside).and_then(|_| std::fs::rename(original, &backup));
        if let Err(e) = moved {
            done.rollback();
            return cleanup(e.into());
        }
        done.backups.push((backup, original.clone()));
    }
    // 3. Rename staged files into place.
    for (_, rel, _) in jobs {
        let dest = base.join(rel);
        let res = dest
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::rename(stage.join(rel), &dest));
        if let Err(e) = res {
            done.rollback();
            return cleanup(e.into());
        }
        done.placed.push(dest);
    }
    // Backups move out of the stage so it can be removed; they stay inside the artist folder.
    let mut moved_backups = Vec::new();
    for (backup, original) in done.backups.drain(..) {
        let keep = base.join(format!(
            ".reelix-replaced-{}",
            backup
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        match std::fs::rename(&backup, &keep) {
            Ok(()) => moved_backups.push((keep, original)),
            Err(_) => moved_backups.push((backup, original)),
        }
    }
    done.backups = moved_backups;
    Ok(done)
}

impl MusicService {
    /// Front cover from the Cover Art Archive (`None` when missing or unreachable).
    pub(crate) async fn fetch_cover(&self, album: &AlbumDto) -> Option<Cover> {
        let art = match self.metadata.cover_art(&album.musicbrainz_release_group_id).await {
            Ok(Some(a)) => a,
            Ok(None) => return None,
            Err(e) => {
                tracing::debug!(album_id = album.id, error = %e, "cover art lookup failed");
                return None;
            }
        };
        let url = art.front.or(art.thumb_500)?;
        let res = match self.http.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::debug!(url, status = %r.status(), "cover download failed");
                return None;
            }
            Err(e) => {
                tracing::debug!(url, error = %e, "cover download failed");
                return None;
            }
        };
        if res.content_length().is_some_and(|l| l as usize > MAX_COVER_BYTES) {
            return None;
        }
        let bytes = res.bytes().await.ok()?;
        if bytes.len() > MAX_COVER_BYTES {
            return None;
        }
        Cover::from_bytes(bytes.to_vec())
    }
}
