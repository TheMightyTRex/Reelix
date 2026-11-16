//! Disk scans: audio files under an artist folder → track files linked to album tracks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use reelix_api_types::music::{AlbumDto, ArtistDto};
use reelix_import::matching::similarity;
use reelix_parser::QualityModel;

use crate::convert::clean_name;
use crate::error::{MusicError, Result};
use crate::matching::{MatchDecision, Preferences, SCAN_ACCEPT_DISTANCE, decide};
use crate::repo::files::FileRow;
use crate::tags::{self, AudioInfo};
use crate::{MusicService, ScanSummary, repo};

/// Audio files under `dir` (or `dir` itself), sorted; hidden and staging folders are skipped.
pub(crate) fn collect_audio(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if dir.is_file() {
        if tags::is_audio(dir) {
            out.push(dir.to_path_buf());
        }
        return out;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.ends_with(".reelix-partial") {
                continue;
            }
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() && tags::is_audio(&p) => out.push(p),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// Reads every file (unreadable ones are skipped).
pub(crate) fn read_all(paths: &[PathBuf]) -> Vec<AudioInfo> {
    paths
        .iter()
        .filter_map(|p| match tags::read(p) {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::debug!(path = %p.display(), error = %e, "skipping audio file");
                None
            }
        })
        .collect()
}

pub(crate) async fn read_dir_audio(dir: PathBuf) -> Result<Vec<AudioInfo>> {
    tokio::task::spawn_blocking(move || read_all(&collect_audio(&dir)))
        .await
        .map_err(|e| MusicError::Internal(e.to_string()))
}

fn is_disc_folder(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["cd", "disc", "disk", "dvd"].iter().any(|p| {
        n.strip_prefix(p).is_some_and(|r| {
            r.trim()
                .trim_start_matches(['-', '_', '.'])
                .trim()
                .parse::<u32>()
                .is_ok()
        })
    })
}

/// The album folder of a file: its parent, or the grandparent for `CD 1`-style folders.
pub(crate) fn album_folder(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or(path);
    match parent.file_name().and_then(|n| n.to_str()) {
        Some(n) if is_disc_folder(n) => parent.parent().unwrap_or(parent).to_path_buf(),
        _ => parent.to_path_buf(),
    }
}

/// `1997 - OK Computer (Remastered) [FLAC]` → `OK Computer (Remastered)`-ish key material.
fn folder_title(folder: &Path) -> String {
    let name = folder.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let mut s = name.to_owned();
    // drop [..] blocks and a "(YYYY)" suffix, and a "YYYY - " prefix
    while let (Some(a), Some(b)) = (s.find('['), s.find(']')) {
        if b <= a {
            break;
        }
        s.replace_range(a..=b, "");
    }
    let t = s.trim();
    let t = match t.char_indices().take(4).all(|(_, c)| c.is_ascii_digit()) && t.len() > 7 {
        true => t[4..].trim_start_matches([' ', '-', '.', '_']),
        false => t,
    };
    let t = t.trim();
    match t.rfind('(') {
        Some(i) if t[i..].len() == 6 && t[i + 1..i + 5].chars().all(|c| c.is_ascii_digit()) => {
            t[..i].trim().to_owned()
        }
        _ => t.to_owned(),
    }
}

fn majority<'a>(values: impl Iterator<Item = Option<&'a str>>) -> Option<&'a str> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for v in values.flatten() {
        *counts.entry(v).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(v, _)| v)
}

impl MusicService {
    /// Which album of `artist` a folder of files belongs to.
    pub(crate) async fn identify_album(
        &self,
        albums: &[AlbumDto],
        folder: &Path,
        files: &[AudioInfo],
        known: &HashMap<String, i64>,
    ) -> Result<Option<i64>> {
        if let Some(rg) = majority(
            files
                .iter()
                .map(|f| f.tags.musicbrainz_release_group_id.as_deref()),
        ) && let Some(a) = albums.iter().find(|a| a.musicbrainz_release_group_id == rg)
        {
            return Ok(Some(a.id));
        }
        if let Some(rel) = majority(files.iter().map(|f| f.tags.musicbrainz_release_id.as_deref()))
            && let Some((_, album_id)) = repo::releases::album_of_mbid(&self.db, rel).await?
            && albums.iter().any(|a| a.id == album_id)
        {
            return Ok(Some(album_id));
        }
        if let Some(id) = files
            .iter()
            .find_map(|f| known.get(&f.path.to_string_lossy().into_owned()))
        {
            return Ok(Some(*id));
        }
        let tag_title = majority(files.iter().map(|f| f.tags.album.as_deref())).map(str::to_owned);
        let names: Vec<String> = tag_title.into_iter().chain([folder_title(folder)]).collect();
        let mut best: Option<(i64, f64)> = None;
        for a in albums {
            for n in &names {
                let sim = if clean_name(n) == clean_name(&a.title) {
                    1.0
                } else {
                    similarity(n, &a.title)
                };
                if sim >= 0.85 && best.is_none_or(|(_, s)| sim > s) {
                    best = Some((a.id, sim));
                }
            }
        }
        Ok(best.map(|(id, _)| id))
    }

    pub async fn scan_artists(&self, ids: &[i64]) -> Result<ScanSummary> {
        let mut total = ScanSummary::default();
        let ids: Vec<i64> = if ids.is_empty() {
            repo::artists::load(&self.db, None)
                .await?
                .into_iter()
                .map(|a| a.id)
                .collect()
        } else {
            ids.to_vec()
        };
        for id in ids {
            let artist = repo::artists::require(&self.db, id).await?;
            total += self.scan_artist(&artist).await?;
            self.emit("artist", "updated", id);
        }
        Ok(total)
    }

    async fn scan_artist(&self, artist: &ArtistDto) -> Result<ScanSummary> {
        let mut summary = ScanSummary::default();
        let base = PathBuf::from(&artist.path);
        let existing = repo::files::of_artist(&self.db, artist.id).await?;
        if !base.is_dir() {
            // Only forget files when the root folder itself is reachable (not an unmounted disk).
            let root_ok = self
                .music_root(artist.root_folder_id)
                .await
                .is_ok_and(|r| Path::new(&r.path).is_dir());
            if root_ok {
                for f in &existing {
                    repo::files::delete(&self.db, f.id).await?;
                    summary.removed += 1;
                }
            }
            return Ok(summary);
        }
        let infos = read_dir_audio(base.clone()).await?;
        let known: HashMap<String, i64> = existing
            .iter()
            .map(|f| {
                (
                    base.join(&f.relative_path).to_string_lossy().into_owned(),
                    f.album_id,
                )
            })
            .collect();
        let existing_paths: HashSet<(i64, String)> = existing
            .iter()
            .map(|f| (f.album_id, f.relative_path.clone()))
            .collect();
        let mut groups: BTreeMap<PathBuf, Vec<AudioInfo>> = BTreeMap::new();
        for i in infos {
            groups.entry(album_folder(&i.path)).or_default().push(i);
        }
        let mut albums = repo::albums::of_artists(&self.db, &[artist.id]).await?;
        let settings = self.settings().await?;
        let mut seen_paths: HashSet<String> = HashSet::new();
        for (folder, files) in groups {
            let album_id = match self.identify_album(&albums, &folder, &files, &known).await? {
                Some(id) => id,
                // A custom artist has no album list from a source: the folder makes one.
                None if artist.custom => {
                    let tag = majority(files.iter().map(|f| f.tags.album.as_deref())).map(str::to_owned);
                    let title = tag.unwrap_or_else(|| folder_title(&folder));
                    match self.custom_album(artist, title.trim()).await? {
                        Some(a) => {
                            let id = a.id;
                            albums.push(a);
                            id
                        }
                        None => continue,
                    }
                }
                None => {
                    tracing::debug!(folder = %folder.display(), "no album matches folder");
                    continue;
                }
            };
            self.ensure_releases_quiet(album_id).await;
            let album = repo::albums::require(&self.db, album_id).await?;
            let candidates = self.candidates(album_id).await?;
            let prefs = Preferences {
                countries: settings.preferred_release_countries.clone(),
                formats: settings.preferred_formats.clone(),
                selected: album.selected_release_id,
            };
            let mapping: Vec<(usize, i64)> =
                match decide(&files, &candidates, &prefs, SCAN_ACCEPT_DISTANCE, false) {
                    MatchDecision::Matched(s) => {
                        self.select_release(&album, s.release_id).await?;
                        s.mapping
                    }
                    _ => Vec::new(),
                };
            repo::tracks::unlink_album(&self.db, album_id).await?;
            let mut by_file: HashMap<usize, Vec<i64>> = HashMap::new();
            for (fi, t) in mapping {
                by_file.entry(fi).or_default().push(t);
            }
            for (fi, info) in files.iter().enumerate() {
                let Ok(rel) = info.path.strip_prefix(&base) else {
                    continue;
                };
                let rel = rel.to_string_lossy().into_owned();
                seen_paths.insert(rel.clone());
                for old in existing
                    .iter()
                    .filter(|f| f.relative_path == rel && f.album_id != album_id)
                {
                    repo::files::delete(&self.db, old.id).await?;
                }
                let row = file_row(album_id, &rel, info, None, None);
                if existing_paths.contains(&(album_id, rel.clone())) {
                    summary.updated += 1;
                } else {
                    summary.added += 1;
                }
                let file_id = repo::files::upsert(&self.db, &row).await?;
                if let Some(tracks) = by_file.get(&fi) {
                    repo::tracks::link_file(&self.db, tracks, Some(file_id)).await?;
                }
            }
        }
        for f in &existing {
            if !seen_paths.contains(&f.relative_path) && !base.join(&f.relative_path).is_file() {
                repo::files::delete(&self.db, f.id).await?;
                summary.removed += 1;
            }
        }
        Ok(summary)
    }
}

/// A `track_files` row for an audio file at `rel` (relative to the artist folder).
pub(crate) fn file_row(
    album_id: i64,
    rel: &str,
    info: &AudioInfo,
    scene_name: Option<String>,
    original_path: Option<String>,
) -> FileRow {
    FileRow {
        id: 0,
        album_id,
        relative_path: rel.to_owned(),
        size: info.size,
        quality: QualityModel::new(info.quality()),
        bitrate_kbps: info.bitrate_kbps,
        sample_rate_hz: info.sample_rate_hz,
        bit_depth: info.bit_depth,
        channels: info.channels,
        duration_ms: info.duration_ms,
        tags: serde_json::to_value(&info.tags).unwrap_or_default(),
        release_group: None,
        scene_name,
        original_path,
        custom_format_ids: Vec::new(),
        custom_format_score: 0,
        date_added: chrono::Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders() {
        assert_eq!(
            album_folder(Path::new("/m/A/OK Computer (1997)/CD 2/01.flac")),
            PathBuf::from("/m/A/OK Computer (1997)")
        );
        assert_eq!(
            album_folder(Path::new("/m/A/OK Computer/01.flac")),
            PathBuf::from("/m/A/OK Computer")
        );
        assert_eq!(
            folder_title(Path::new("/m/OK Computer (1997) [FLAC]")),
            "OK Computer"
        );
        assert_eq!(folder_title(Path::new("/m/1997 - OK Computer")), "OK Computer");
        assert!(is_disc_folder("Disc 1") && is_disc_folder("CD2") && !is_disc_folder("CDs"));
    }
}
