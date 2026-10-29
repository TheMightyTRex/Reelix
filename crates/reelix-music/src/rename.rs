//! Renaming track files to the current naming settings.
//!
//! [`RenamePreviewDto`] is shared with movies/series: for music `kind` is a placeholder
//! (`movie`), `media_id` is the artist id, `season_number` the disc and `episode_numbers`
//! the track numbers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use reelix_api_types::library::RenamePreviewDto;
use reelix_core::MediaKind;
use reelix_db::NewJob;

use crate::error::{MusicError, Result};
use crate::import_api::facts;
use crate::naming::{codec_name, track_path};
use crate::tags::{AudioInfo, FileTags, extension};
use crate::{MusicService, kinds, repo};

impl MusicService {
    pub async fn rename_preview(&self, artist_id: i64) -> Result<Vec<RenamePreviewDto>> {
        let artist = repo::artists::require(&self.db, artist_id).await?;
        let settings = self.settings().await?;
        let cfg = self.naming_config().await?;
        let mut out = Vec::new();
        for album in repo::albums::of_artists(&self.db, &[artist_id]).await? {
            let Some(rid) = album.selected_release_id else {
                continue;
            };
            let Some(release) = repo::releases::get(&self.db, rid).await? else {
                continue;
            };
            let tracks = repo::tracks::of_release(&self.db, rid).await?;
            let files: HashMap<i64, _> = repo::files::of_albums(&self.db, &[album.id])
                .await?
                .into_iter()
                .map(|f| (f.id, f))
                .collect();
            let disc_count = release
                .disc_count
                .max(tracks.iter().map(|t| t.disc_number).max().unwrap_or(1));
            let mut done = std::collections::HashSet::new();
            for t in &tracks {
                let Some(f) = t.track_file_id.and_then(|id| files.get(&id)) else {
                    continue;
                };
                if !done.insert(f.id) {
                    continue;
                }
                let ext = extension(Path::new(&f.relative_path)).unwrap_or_default();
                let info = AudioInfo {
                    path: PathBuf::from(&f.relative_path),
                    ext: ext.clone(),
                    size: f.size,
                    duration_ms: f.duration_ms,
                    bitrate_kbps: f.bitrate_kbps,
                    sample_rate_hz: f.sample_rate_hz,
                    bit_depth: f.bit_depth,
                    channels: f.channels,
                    tags: FileTags::default(),
                };
                let mut fx = facts(&artist, &album, Some(&release), t, &info);
                fx.quality = Some(f.quality.quality);
                fx.audio_codec = codec_name(&ext, f.quality.quality);
                let stem = Path::new(&f.relative_path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned());
                let new = track_path(&settings.naming, &fx, disc_count, &ext, stem.as_deref(), &cfg)
                    .to_string_lossy()
                    .into_owned();
                if new != f.relative_path {
                    out.push(RenamePreviewDto {
                        kind: MediaKind::Movie,
                        media_id: artist_id,
                        file_id: f.id,
                        season_number: Some(t.disc_number),
                        episode_numbers: vec![t.track_number],
                        existing_path: f.relative_path.clone(),
                        new_path: new,
                    });
                }
            }
        }
        Ok(out)
    }

    pub async fn queue_rename(&self, artist_id: i64) -> Result<Option<i64>> {
        repo::artists::require(&self.db, artist_id).await?;
        self.enqueue(
            NewJob::new(
                kinds::MUSIC_RENAME,
                serde_json::json!({ "artist_ids": [artist_id] }),
            )
            .dedupe(format!("music_rename:{artist_id}")),
        )
        .await
    }

    /// Renames the files of an artist (job handler); returns how many were renamed.
    pub async fn execute_rename(&self, artist_id: i64) -> Result<usize> {
        let artist = repo::artists::require(&self.db, artist_id).await?;
        let base = PathBuf::from(&artist.path);
        let guard = self.path_guard().await?;
        let mut n = 0;
        for p in self.rename_preview(artist_id).await? {
            let from = base.join(&p.existing_path);
            let to = base.join(&p.new_path);
            guard.check(&to)?;
            if !from.is_file() {
                continue;
            }
            if to.exists() {
                tracing::warn!(path = %to.display(), "rename target exists; skipped");
                continue;
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&from, &to).map_err(|e| MusicError::Files(reelix_import::Error::io(&from, e)))?;
            repo::files::set_relative_path(&self.db, p.file_id, &p.new_path).await?;
            if let Some(parent) = from.parent() {
                Self::prune_empty_dirs(parent, &base);
            }
            n += 1;
        }
        if n > 0 {
            self.emit("artist", "updated", artist_id);
        }
        Ok(n)
    }
}
