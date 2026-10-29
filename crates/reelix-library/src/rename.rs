//! Rename preview/execution using the naming templates.

use std::path::{Path, PathBuf};

use reelix_api_types::library::{RenamePreviewDto, RenamePreviewQuery, RenameRequest};
use reelix_core::MediaKind;
use reelix_db::NewJob;
use reelix_import::files::{TransferMode, transfer};
use reelix_import::naming::{EpisodeNaming, FileNaming};
use reelix_parser::parse_path;

use crate::LibraryService;
use crate::error::{LibraryError, Result};
use crate::kinds;
use crate::movies::movie_naming;
use crate::repo::files::{self, FileRow, FileTable};
use crate::repo::{movies, series as repo};
use crate::series::series_naming;
use crate::service::ProfileCache;

fn file_naming(f: &FileRow, cache: &ProfileCache) -> FileNaming {
    let source = f.scene_name.clone().unwrap_or_else(|| f.relative_path.clone());
    let parsed = parse_path(&source);
    let mut n = FileNaming::from_parsed(&parsed, f.media_info.as_ref());
    n.quality = f.quality;
    n.release_group = f.release_group.clone().or(n.release_group);
    n.edition = f.edition.clone().or(n.edition);
    n.custom_formats = f
        .custom_format_ids
        .iter()
        .filter(|id| {
            cache
                .formats
                .iter()
                .any(|cf| cf.id == **id && cf.include_in_rename)
        })
        .filter_map(|id| cache.format_names.get(id).cloned())
        .collect();
    n.original_file_name = Path::new(&f.relative_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned());
    n
}

fn ext_of(rel: &str) -> String {
    Path::new(rel)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mkv".into())
}

impl LibraryService {
    /// Files of a movie or series (optionally one season) whose names differ from the templates.
    pub async fn rename_preview(&self, q: &RenamePreviewQuery) -> Result<Vec<RenamePreviewDto>> {
        match (q.movie_id, q.series_id) {
            (Some(id), _) => self.movie_renames(id).await,
            (None, Some(id)) => self.series_renames(id, q.season).await,
            _ => Err(LibraryError::validation("movie_id or series_id is required")),
        }
    }

    async fn movie_renames(&self, id: i64) -> Result<Vec<RenamePreviewDto>> {
        let cache = self.profiles().await?;
        let m = movies::get(&self.db, id, &cache)
            .await?
            .ok_or_else(|| LibraryError::not_found("movie", id))?;
        let naming = self.naming_config().await?;
        let mut out = Vec::new();
        for f in files::list(&self.db, FileTable::Movie, Some(&[id])).await? {
            let name = naming.movie_file_name(&movie_naming(&m), &file_naming(&f, &cache))?;
            let new_rel = format!("{name}.{}", ext_of(&f.relative_path));
            if new_rel != f.relative_path {
                out.push(RenamePreviewDto {
                    kind: MediaKind::Movie,
                    media_id: id,
                    file_id: f.id,
                    season_number: None,
                    episode_numbers: Vec::new(),
                    existing_path: f.relative_path.clone(),
                    new_path: new_rel,
                });
            }
        }
        Ok(out)
    }

    async fn series_renames(&self, id: i64, season: Option<u32>) -> Result<Vec<RenamePreviewDto>> {
        let cache = self.profiles().await?;
        let s = self.series_row(id).await?;
        let naming = self.naming_config().await?;
        let sn = series_naming(&s);
        let eps = repo::episodes(&self.db, id, None).await?;
        let mut out = Vec::new();
        for f in files::list(&self.db, FileTable::Episode, Some(&[id])).await? {
            let mut fe: Vec<_> = eps.iter().filter(|e| e.episode_file_id == Some(f.id)).collect();
            if fe.is_empty() || season.is_some_and(|x| fe[0].season_number != x) {
                continue;
            }
            fe.sort_by_key(|e| (e.season_number, e.episode_number));
            let en: Vec<EpisodeNaming> = fe
                .iter()
                .map(|e| EpisodeNaming {
                    season: e.season_number,
                    episode: e.episode_number,
                    absolute: e.absolute_number,
                    title: e.title.clone().unwrap_or_default(),
                    air_date: e.air_date,
                })
                .collect();
            let ext = ext_of(&f.relative_path);
            let full = naming.episode_path(Path::new(""), &sn, &en, &file_naming(&f, &cache), &ext)?;
            // `episode_path` = <series folder>/<season folder>/<file>; drop the series folder.
            let mut comps: Vec<String> = full
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if !comps.is_empty() {
                comps.remove(0);
            }
            if !s.season_folder && comps.len() > 1 {
                comps.drain(..comps.len() - 1);
            }
            let new_rel = comps.iter().collect::<PathBuf>().to_string_lossy().into_owned();
            if new_rel != f.relative_path {
                out.push(RenamePreviewDto {
                    kind: MediaKind::Series,
                    media_id: id,
                    file_id: f.id,
                    season_number: Some(fe[0].season_number),
                    episode_numbers: fe.iter().map(|e| e.episode_number).collect(),
                    existing_path: f.relative_path.clone(),
                    new_path: new_rel,
                });
            }
        }
        Ok(out)
    }

    pub async fn queue_rename(&self, req: RenameRequest) -> Result<Option<i64>> {
        if req.ids.is_empty() {
            return Err(LibraryError::validation("`ids` must not be empty"));
        }
        self.enqueue(NewJob::new(kinds::RENAME_FILES, serde_json::to_value(&req)?).priority(5))
            .await
    }

    /// Renames the files of `req`; returns how many were renamed.
    pub async fn execute_rename(&self, req: &RenameRequest) -> Result<usize> {
        let guard = self.path_guard().await?;
        let mut n = 0;
        for id in &req.ids {
            let (previews, base) = match req.kind {
                MediaKind::Movie => {
                    let cache = self.profiles().await?;
                    let Some(m) = movies::get(&self.db, *id, &cache).await? else {
                        continue;
                    };
                    (self.movie_renames(*id).await?, m.path)
                }
                MediaKind::Series => {
                    let Ok(s) = self.series_row(*id).await else {
                        continue;
                    };
                    (self.series_renames(*id, None).await?, s.path)
                }
            };
            let table = match req.kind {
                MediaKind::Movie => FileTable::Movie,
                MediaKind::Series => FileTable::Episode,
            };
            for p in previews {
                if req.file_ids.as_ref().is_some_and(|ids| !ids.contains(&p.file_id)) {
                    continue;
                }
                let src = Path::new(&base).join(&p.existing_path);
                let dst = Path::new(&base).join(&p.new_path);
                guard.check_existing(&src)?;
                guard.check(&dst)?;
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                transfer(&src, &dst, TransferMode::Move)?;
                files::set_relative_path(&self.db, table, p.file_id, &p.new_path, p.season_number).await?;
                // Remove the old (now empty) season folder.
                if let Some(old_dir) = src.parent()
                    && old_dir != Path::new(&base)
                {
                    let _ = std::fs::remove_dir(old_dir);
                }
                n += 1;
                tracing::info!(from = %src.display(), to = %dst.display(), "renamed");
            }
            let entity = if req.kind == MediaKind::Movie {
                "movie"
            } else {
                "series"
            };
            self.emit(entity, "renamed", *id);
        }
        Ok(n)
    }
}
