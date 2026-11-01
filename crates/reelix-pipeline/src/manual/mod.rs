//! Manual import: preview a folder (or a queue item's output) and import user-assigned files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use reelix_api_types::library::ImportMode;
use reelix_api_types::pipeline::{ManualImportItemDto, ManualImportQuery, ManualImportRequest};
use reelix_core::Protocol;
use reelix_db::NewJob;
use reelix_import::files::{PathGuard, TransferMode};

use crate::import::{FileImporter, Hint, PlannedFile, Target, parse_mode, transfer_mode};
use crate::monitor::views::Refs;
use crate::monitor::{ActivityError, ActivityService, DownloadRef, Result, kinds};
use crate::store::tracked::{self, TrackedDownload, TrackedState};

/// Outcome of [`ActivityService::manual_import`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManualImportReport {
    pub imported: usize,
    /// `path: reason` for every file that was not imported.
    pub failures: Vec<String>,
}

impl ActivityService {
    /// Folders manual import may read: root folders, client output folders, mapped paths and
    /// the output folders of tracked downloads.
    async fn manual_guard(&self) -> Result<PathGuard> {
        let mut roots = self.providers.download_roots().await;
        for t in tracked::active(&self.db).await? {
            if let Some(p) = t.output_path {
                let p = PathBuf::from(p);
                roots.push(if p.is_dir() {
                    p
                } else {
                    p.parent().map(Path::to_path_buf).unwrap_or(p)
                });
            }
        }
        Ok(self.library.importer().guard(&roots).await?)
    }

    fn hint_for(t: &TrackedDownload) -> Hint {
        Hint {
            movie_id: t.movie_id,
            series_id: if t.movie_id.is_some() { None } else { t.series_id },
            episode_ids: t.episode_ids.clone(),
            release_title: Some(t.title.clone()),
        }
    }

    /// `GET /manualimport`: the videos of a folder (or queue item) with their proposed mapping.
    pub async fn manual_import_preview(&self, q: &ManualImportQuery) -> Result<Vec<ManualImportItemDto>> {
        let (folder, mut hint, download_id) = match (&q.download_id, &q.folder) {
            (Some(d), _) if !d.trim().is_empty() => {
                let t = tracked::by_download_id(&self.db, d)
                    .await?
                    .ok_or_else(|| ActivityError::not_found("download", d))?;
                if !t.content.is_empty() {
                    return Err(ActivityError::validation(
                        "manual import handles movies and episodes; retry the import of this book / album \
                         download from the queue instead",
                    ));
                }
                let path = t
                    .output_path
                    .clone()
                    .ok_or_else(|| ActivityError::validation("the download has no output path yet"))?;
                (PathBuf::from(path), Self::hint_for(&t), Some(t.download_id))
            }
            (_, Some(f)) if !f.trim().is_empty() => {
                let path = PathBuf::from(f.trim());
                self.manual_guard().await?.check_existing(&path).map_err(|_| {
                    ActivityError::validation(
                        "folder must exist inside a root folder, a download client folder or a mapped path",
                    )
                })?;
                (path, Hint::default(), None)
            }
            _ => return Err(ActivityError::validation("folder or download_id is required")),
        };
        if q.movie_id.is_some() || q.series_id.is_some() {
            let series = if q.movie_id.is_some() { None } else { q.series_id };
            if hint.movie_id != q.movie_id || hint.series_id != series {
                hint.episode_ids.clear();
            }
            hint.movie_id = q.movie_id;
            hint.series_id = series;
        }
        if !folder.exists() {
            return Err(ActivityError::validation(format!(
                "{} does not exist",
                folder.display()
            )));
        }
        let fi = FileImporter::new(&self.library).await?;
        let planned = fi.plan(&folder, &hint).await?;
        self.preview_dtos(&planned, download_id).await
    }

    async fn preview_dtos(
        &self,
        planned: &[PlannedFile],
        download_id: Option<String>,
    ) -> Result<Vec<ManualImportItemDto>> {
        let mut movies = Vec::new();
        let mut series = Vec::new();
        let mut episodes = Vec::new();
        for t in planned.iter().filter_map(|p| p.target.as_ref()) {
            match t {
                Target::Movie(id) => movies.push(*id),
                Target::Episodes {
                    series_id,
                    episode_ids,
                } => {
                    series.push(*series_id);
                    episodes.extend(episode_ids.iter().copied());
                }
            }
        }
        let refs = Refs::load(self, movies, series, episodes).await?;
        Ok(planned
            .iter()
            .map(|p| {
                let (movie_id, series_id) = match &p.target {
                    Some(Target::Movie(id)) => (Some(*id), None),
                    Some(Target::Episodes { series_id, .. }) => (None, Some(*series_id)),
                    None => (None, None),
                };
                let episodes = refs.episodes(p.target.as_ref().map_or(&[][..], Target::episode_ids));
                ManualImportItemDto {
                    path: p.path.to_string_lossy().into_owned(),
                    relative_path: p.relative.to_string_lossy().into_owned(),
                    size: p.size,
                    quality: p.quality,
                    languages: p.languages.clone(),
                    release_group: p.release_group.clone(),
                    media: refs.media(movie_id, series_id),
                    season_number: episodes.first().map(|e| e.season_number),
                    episodes,
                    rejections: p.rejections.clone(),
                    download_id: download_id.clone(),
                }
            })
            .collect())
    }

    /// `POST /manualimport`: validates the request and queues job `manual_import`.
    pub async fn queue_manual_import(&self, req: &ManualImportRequest) -> Result<Option<i64>> {
        if req.files.is_empty() {
            return Err(ActivityError::validation("`files` must not be empty"));
        }
        if parse_mode(&req.import_mode).is_none() {
            return Err(ActivityError::validation(
                "import_mode must be auto, move, copy or hardlink",
            ));
        }
        for f in &req.files {
            if f.movie_id.is_none() && (f.series_id.is_none() || f.episode_ids.is_empty()) {
                return Err(ActivityError::validation(format!(
                    "{}: choose a movie, or a series and its episodes",
                    f.path
                )));
            }
        }
        self.enqueue(NewJob::new(kinds::MANUAL_IMPORT, serde_json::to_value(req)?).priority(5))
            .await
    }

    /// Imports user-assigned files (job body of `manual_import`). Rejections other than a
    /// missing destination are overridden: the user chose these files.
    pub async fn manual_import(&self, req: &ManualImportRequest) -> Result<ManualImportReport> {
        let mode_override =
            parse_mode(&req.import_mode).ok_or_else(|| ActivityError::validation("invalid import_mode"))?;
        let guard = self.manual_guard().await?;
        let fi = FileImporter::new(&self.library).await?;
        let mut report = ManualImportReport::default();
        // download id → (tracked row, all files ok)
        let mut downloads: BTreeMap<String, (TrackedDownload, bool)> = BTreeMap::new();
        for f in &req.files {
            let path = PathBuf::from(&f.path);
            let fail = |report: &mut ManualImportReport, msg: String| {
                report.failures.push(format!("{}: {msg}", f.path))
            };
            let tracked_row = match &f.download_id {
                Some(d) => tracked::by_download_id(&self.db, d).await?,
                None => None,
            };
            if guard.check_existing(&path).is_err()
                && tracked_row
                    .as_ref()
                    .and_then(|t| t.output_path.as_ref())
                    .is_none_or(|o| !inside(&path, Path::new(o)))
            {
                fail(&mut report, "file is not inside an allowed folder".into());
                mark_failed(&mut downloads, tracked_row);
                continue;
            }
            let target = match (f.movie_id, f.series_id) {
                (Some(id), _) => Target::Movie(id),
                (None, Some(series_id)) if !f.episode_ids.is_empty() => Target::Episodes {
                    series_id,
                    episode_ids: f.episode_ids.clone(),
                },
                _ => {
                    fail(&mut report, "no movie or episodes chosen".into());
                    mark_failed(&mut downloads, tracked_row);
                    continue;
                }
            };
            let hint = tracked_row.as_ref().map(Self::hint_for).unwrap_or_default();
            let mut planned = match fi.plan_file(&path, target, &hint).await {
                Ok(p) => p,
                Err(e) => {
                    fail(&mut report, e.to_string());
                    mark_failed(&mut downloads, tracked_row);
                    continue;
                }
            };
            if let Some(q) = f.quality {
                planned.quality = q;
                // Re-evaluate the destination with the chosen quality.
                planned.rejections.clear();
                fi.evaluate(&mut planned).await?;
            }
            if !f.languages.is_empty() {
                planned.languages = f.languages.clone();
            }
            if f.release_group.is_some() {
                planned.release_group = f.release_group.clone();
            }
            if planned.destination.is_none() {
                fail(&mut report, planned.rejections.join("; "));
                mark_failed(&mut downloads, tracked_row);
                continue;
            }
            let mode = match mode_override {
                Some(m) => m,
                None => match &tracked_row {
                    Some(t) => transfer_mode(fi.settings.import_mode, t.protocol == Protocol::Usenet, false),
                    None => match fi.settings.import_mode {
                        ImportMode::Copy => TransferMode::Copy,
                        ImportMode::Move => TransferMode::Move,
                        ImportMode::HardlinkOrCopy => TransferMode::HardlinkOrCopy,
                    },
                },
            };
            let mut roots: Vec<PathBuf> = guard.roots().to_vec();
            if let Some(o) = tracked_row.as_ref().and_then(|t| t.output_path.as_ref()) {
                roots.push(PathBuf::from(o));
            }
            match fi.execute(&planned, mode, &roots).await {
                Ok(item) => {
                    report.imported += 1;
                    let r = match &tracked_row {
                        Some(t) => self.download_ref(t),
                        None => DownloadRef {
                            title: planned.scene_name.clone().unwrap_or_else(|| f.path.clone()),
                            ..Default::default()
                        },
                    };
                    self.record_import(&item, &r).await;
                    if let Some(t) = tracked_row {
                        if t.movie_id.is_none() && t.series_id.is_none() {
                            let (m, s) = match &item.target {
                                Target::Movie(id) => (Some(*id), None),
                                Target::Episodes { series_id, .. } => (None, Some(*series_id)),
                            };
                            tracked::set_media(&self.db, t.id, m, s, item.target.episode_ids()).await?;
                        }
                        downloads.entry(t.download_id.clone()).or_insert((t, true));
                    }
                }
                Err(e) => {
                    fail(&mut report, e.to_string());
                    mark_failed(&mut downloads, tracked_row);
                }
            }
        }
        for (_, (t, ok)) in downloads {
            if ok && t.state.is_active() {
                tracked::set_state(&self.db, t.id, TrackedState::Imported, &[]).await?;
                self.emit("queue", "updated", t.id);
            }
        }
        Ok(report)
    }
}

fn mark_failed(downloads: &mut BTreeMap<String, (TrackedDownload, bool)>, t: Option<TrackedDownload>) {
    if let Some(t) = t {
        downloads
            .entry(t.download_id.clone())
            .and_modify(|e| e.1 = false)
            .or_insert((t, false));
    }
}

/// Whether `path` is really inside `root`: both are resolved first, so `..` segments and
/// symlinks can't reach files outside the download folder (a lexical `starts_with` can be
/// fooled by `<root>/../../secret.key`).
fn inside(path: &Path, root: &Path) -> bool {
    match (std::fs::canonicalize(path), std::fs::canonicalize(root)) {
        (Ok(p), Ok(r)) => p.starts_with(r),
        _ => false,
    }
}
