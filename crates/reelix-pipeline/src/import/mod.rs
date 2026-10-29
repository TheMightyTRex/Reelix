//! Import engine: scan a download folder, map its files to library media, check the import
//! specifications (sample, unpacking, upgrade, already imported, free space), transfer the
//! files into the library (old files → recycle bin), copy extra files and record everything.
//!
//! [`FileImporter::plan`] is shared by automatic import (download monitor) and the manual import
//! preview; [`FileImporter::execute`] performs one planned file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reelix_api_types::library::{ImportMode, MediaManagementSettings};
use reelix_core::MediaKind;
use reelix_decision::engine::Scored;
use reelix_import::files::{
    FileKind, FoundFile, PermissionSettings, SampleRules, ScanOptions, TransferMode, TransferOptions,
    detect_sample, disk_space, extension, scan_async, transfer_with,
};
use reelix_import::matching::{best_movie_match, match_parsed_episodes};
use reelix_import::mediainfo::{MediaInfo, ProbeConfig, probe};
use reelix_library::LibraryService;
use reelix_library::import_api::{
    ExistingFile, FileScore, ImportedFile, MediaCandidate, MovieTarget, NamingInput, SeriesTarget,
};
use reelix_parser::{Language, ParsedRelease, Quality, QualityModel, parse, parse_path};

use crate::monitor::error::{ActivityError, Result};

/// What a file is imported as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Movie(i64),
    Episodes { series_id: i64, episode_ids: Vec<i64> },
}

impl Target {
    pub fn kind(&self) -> MediaKind {
        match self {
            Self::Movie(_) => MediaKind::Movie,
            Self::Episodes { .. } => MediaKind::Series,
        }
    }

    pub fn media_id(&self) -> i64 {
        match self {
            Self::Movie(id) => *id,
            Self::Episodes { series_id, .. } => *series_id,
        }
    }

    pub fn episode_ids(&self) -> &[i64] {
        match self {
            Self::Movie(_) => &[],
            Self::Episodes { episode_ids, .. } => episode_ids,
        }
    }
}

/// What is already known about a download (from the tracked row or the manual import form).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hint {
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    pub episode_ids: Vec<i64>,
    /// Release (download) title; used as scene name and quality fallback.
    pub release_title: Option<String>,
}

/// A video file with its mapping, metadata and import rejections.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedFile {
    pub path: PathBuf,
    /// Relative to the scanned folder.
    pub relative: PathBuf,
    pub size: u64,
    pub parsed: ParsedRelease,
    pub target: Option<Target>,
    pub quality: QualityModel,
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub media_info: Option<MediaInfo>,
    pub score: FileScore,
    /// Library destination (when the target is known and the name renders).
    pub destination: Option<PathBuf>,
    /// Library files this one replaces (upgrades).
    pub replaces: Vec<PathBuf>,
    pub is_upgrade: bool,
    /// Why the file can't be imported automatically.
    pub rejections: Vec<String>,
    /// Subtitles / nfo next to the video, with the suffix kept after the video name.
    pub extras: Vec<(PathBuf, String)>,
    /// Scene name stored with the library file.
    pub scene_name: Option<String>,
}

impl PlannedFile {
    pub fn accepted(&self) -> bool {
        self.target.is_some() && self.rejections.is_empty() && self.destination.is_some()
    }
}

/// One imported file.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedItem {
    pub target: Target,
    pub file_id: i64,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub size: u64,
    pub quality: QualityModel,
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub score: FileScore,
    pub is_upgrade: bool,
    /// Old files moved to the recycle bin (or deleted).
    pub replaced: Vec<PathBuf>,
    pub extras: Vec<PathBuf>,
    pub scene_name: Option<String>,
}

/// Resolves the configured import mode for a download.
pub fn transfer_mode(setting: ImportMode, usenet: bool, can_move: bool) -> TransferMode {
    match (setting, usenet) {
        (ImportMode::Copy, _) => TransferMode::Copy,
        (_, true) => TransferMode::Move,
        (ImportMode::Move, false) if can_move => TransferMode::Move,
        _ => TransferMode::HardlinkOrCopy,
    }
}

/// Parses `auto|move|copy|hardlink` (manual import).
pub fn parse_mode(s: &str) -> Option<Option<TransferMode>> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => None,
        "move" => Some(TransferMode::Move),
        "copy" => Some(TransferMode::Copy),
        "hardlink" => Some(TransferMode::HardlinkOrCopy),
        _ => return None,
    })
}

fn octal(s: Option<&str>) -> Option<u32> {
    s.and_then(|m| u32::from_str_radix(m.trim(), 8).ok())
}

const MIB: u64 = 1024 * 1024;

/// See the module docs.
pub struct FileImporter<'a> {
    pub library: &'a LibraryService,
    pub settings: MediaManagementSettings,
}

impl<'a> FileImporter<'a> {
    pub async fn new(library: &'a LibraryService) -> Result<Self> {
        Ok(Self {
            settings: library.media_management().await?,
            library,
        })
    }

    fn permissions(&self) -> PermissionSettings {
        let s = &self.settings;
        if !s.set_permissions {
            return PermissionSettings::default();
        }
        PermissionSettings {
            file_mode: octal(s.file_mode.as_deref()),
            dir_mode: octal(s.dir_mode.as_deref()),
            uid: s.chown_uid,
            gid: s.chown_gid,
        }
    }

    async fn probe(&self, path: &Path) -> Option<MediaInfo> {
        if !self.settings.analyze_video_files {
            return None;
        }
        let cfg = ProbeConfig {
            ffprobe_path: PathBuf::from(&self.settings.ffprobe_path),
            timeout: Duration::from_secs(30),
        };
        match probe(path, &cfg).await {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "ffprobe unavailable or failed");
                None
            }
        }
    }

    /// Videos (not extras) and the other files below `source` (a folder or a single file).
    pub async fn scan(&self, source: &Path) -> Result<(Vec<FoundFile>, Vec<FoundFile>)> {
        let files = scan_async(
            source.to_path_buf(),
            ScanOptions {
                include_non_video: true,
                max_depth: 8,
            },
        )
        .await?;
        let (videos, others) = files
            .into_iter()
            .partition(|f| f.kind == FileKind::Video && f.extra.is_none());
        Ok((videos, others))
    }

    /// A planned file for one video before mapping (parse, probe, quality, languages).
    async fn describe(&self, f: &FoundFile, source: &Path, hint: &Hint) -> PlannedFile {
        // Parse with the release folder name so obfuscated file names still parse.
        let base = if source.is_dir() {
            source.parent().unwrap_or(source)
        } else {
            source.parent().and_then(Path::parent).unwrap_or(Path::new("/"))
        };
        let rel_for_parse = f.path.strip_prefix(base).unwrap_or(&f.path);
        let parsed = parse_path(&rel_for_parse.to_string_lossy());
        let release = hint.release_title.as_deref().map(parse);
        let info = self.probe(&f.path).await;
        let mut quality = parsed.quality;
        if quality.quality == Quality::Unknown
            && let Some(r) = &release
        {
            quality = r.quality;
        }
        if quality.quality == Quality::Unknown
            && let Some(m) = &info
        {
            quality.quality = Quality::from_parts(parsed.quality.quality.source(), m.resolution());
        }
        let mut languages = info.as_ref().map(MediaInfo::audio_languages).unwrap_or_default();
        languages.retain(|l| *l != Language::Unknown);
        if languages.is_empty() {
            languages = parsed.languages.clone();
        }
        if languages.is_empty()
            && let Some(r) = &release
        {
            languages = r.languages.clone();
        }
        let release_group = parsed
            .release_group
            .clone()
            .or_else(|| release.as_ref().and_then(|r| r.release_group.clone()));
        let mut rejections = Vec::new();
        if f.relative.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            s.starts_with("_UNPACK_") || s.starts_with("_FAILED_")
        }) {
            rejections.push("Download is still being unpacked".to_owned());
        }
        PlannedFile {
            path: f.path.clone(),
            relative: f.relative.clone(),
            size: f.size,
            parsed,
            target: None,
            quality,
            languages,
            release_group,
            media_info: info,
            score: FileScore::default(),
            destination: None,
            replaces: Vec::new(),
            is_upgrade: false,
            rejections,
            extras: Vec::new(),
            scene_name: None,
        }
    }

    fn is_sample(&self, p: &PlannedFile, runtime: Option<u32>) -> bool {
        let rt = p
            .media_info
            .as_ref()
            .and_then(|m| m.duration_secs)
            .filter(|d| *d > 0.0)
            .map(Duration::from_secs_f64);
        detect_sample(&p.path, p.size, rt, runtime, &SampleRules::default()).is_some()
    }

    /// Scans `source` and maps every video: to the hinted movie/series when given, otherwise
    /// by title against the whole library. Every file gets its rejections filled.
    pub async fn plan(&self, source: &Path, hint: &Hint) -> Result<Vec<PlannedFile>> {
        let (videos, others) = self.scan(source).await?;
        let mut planned = Vec::with_capacity(videos.len());
        for v in &videos {
            planned.push(self.describe(v, source, hint).await);
        }
        if let Some(movie_id) = hint.movie_id {
            let mt = self.library.importer().movie_target(movie_id).await?;
            self.map_movie(&mut planned, &mt);
        } else if let Some(series_id) = hint.series_id {
            let st = self.library.importer().series_target(series_id).await?;
            self.map_series(&mut planned, &st, &hint.episode_ids);
        } else {
            self.map_by_title(&mut planned).await?;
        }
        let single = planned.iter().filter(|p| p.target.is_some()).count() == 1;
        for p in &mut planned {
            if p.target.is_some() {
                p.extras = extras_for(&p.path, &others, single, &self.settings);
                if single {
                    p.scene_name = hint.release_title.clone();
                }
                if p.scene_name.is_none() {
                    p.scene_name = p.path.file_stem().map(|s| s.to_string_lossy().into_owned());
                }
            }
            self.evaluate(p).await?;
        }
        Ok(planned)
    }

    /// Plans one explicitly assigned file (manual import).
    pub async fn plan_file(&self, path: &Path, target: Target, hint: &Hint) -> Result<PlannedFile> {
        let meta = tokio::fs::metadata(path).await?;
        if !meta.is_file() {
            return Err(ActivityError::validation(format!(
                "{} is not a file",
                path.display()
            )));
        }
        let found = FoundFile {
            path: path.to_path_buf(),
            relative: PathBuf::from(path.file_name().unwrap_or_default()),
            size: meta.len(),
            kind: FileKind::Video,
            extra: None,
            modified: None,
        };
        let mut p = self.describe(&found, path, hint).await;
        p.target = Some(target);
        let (videos, others) = match path.parent() {
            Some(dir) => self.scan(dir).await.unwrap_or_default(),
            None => Default::default(),
        };
        let single = videos.len() <= 1;
        p.extras = extras_for(path, &others, single, &self.settings);
        p.scene_name = if single { hint.release_title.clone() } else { None }
            .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()));
        self.evaluate(&mut p).await?;
        Ok(p)
    }

    fn map_movie(&self, planned: &mut [PlannedFile], mt: &MovieTarget) {
        let runtime = mt.movie.runtime_minutes;
        for p in planned.iter_mut() {
            if self.is_sample(p, runtime) {
                p.rejections.push("Sample".into());
            }
        }
        // Largest non-sample video is the movie; the rest are rejected.
        let best = planned
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.rejections.iter().any(|r| r == "Sample"))
            .max_by_key(|(_, p)| p.size)
            .map(|(i, _)| i);
        for (i, p) in planned.iter_mut().enumerate() {
            p.target = Some(Target::Movie(mt.movie.id));
            if Some(i) != best && !p.rejections.iter().any(|r| r == "Sample") {
                p.rejections
                    .push("Not the main movie file (a larger video is in the download)".into());
            }
        }
    }

    fn map_series(&self, planned: &mut [PlannedFile], st: &SeriesTarget, hinted: &[i64]) {
        let expected = st.expected();
        let runtime = st.series.runtime_minutes;
        let non_samples: Vec<usize> = (0..planned.len())
            .filter(|i| {
                let s = self.is_sample(&planned[*i], runtime);
                if s {
                    planned[*i].rejections.push("Sample".into());
                }
                !s
            })
            .collect();
        let only_one = non_samples.len() == 1;
        // Larger files claim episodes first.
        let mut order = non_samples.clone();
        order.sort_by_key(|i| std::cmp::Reverse(planned[*i].size));
        let mut claimed: HashSet<i64> = HashSet::new();
        for i in order {
            let p = &mut planned[i];
            let ids = match match_parsed_episodes(&p.parsed, st.series_type, &expected) {
                Ok(m) if !m.episode_ids.is_empty() => m.episode_ids,
                Err(_) if only_one && !hinted.is_empty() => hinted.to_vec(),
                Ok(_) => {
                    p.rejections.push("No episodes matched the file name".into());
                    continue;
                }
                Err(e) => {
                    p.rejections.push(format!("Unable to match episodes: {e}"));
                    continue;
                }
            };
            if ids.iter().any(|e| claimed.contains(e)) {
                p.rejections
                    .push("Duplicate: another file in the download has the same episode".into());
            }
            claimed.extend(ids.iter().copied());
            p.target = Some(Target::Episodes {
                series_id: st.series.id,
                episode_ids: ids,
            });
        }
        for i in non_samples {
            if planned[i].target.is_none() && planned[i].rejections.is_empty() {
                planned[i]
                    .rejections
                    .push("No episodes matched the file name".into());
            }
        }
    }

    /// Maps files without a hint: episodes → series by title, others → movies by title + year.
    async fn map_by_title(&self, planned: &mut [PlannedFile]) -> Result<()> {
        if planned.is_empty() {
            return Ok(());
        }
        let candidates = self.library.importer().candidates().await?;
        let (series, movies): (Vec<&MediaCandidate>, Vec<&MediaCandidate>) =
            candidates.iter().partition(|c| c.kind == MediaKind::Series);
        let series_exp: Vec<_> = series.iter().map(|c| c.expected.clone()).collect();
        let movie_exp: Vec<_> = movies.iter().map(|c| c.expected.clone()).collect();
        let mut by_series: std::collections::BTreeMap<i64, Vec<usize>> = Default::default();
        for (i, p) in planned.iter_mut().enumerate() {
            if p.parsed.episode().is_some() {
                let mut q = p.parsed.clone();
                q.year = None;
                match best_movie_match(&q, &series_exp) {
                    Some((idx, _)) => by_series.entry(series[idx].id).or_default().push(i),
                    None => p.rejections.push(format!("Unknown series: {}", p.parsed.title)),
                }
            } else {
                match best_movie_match(&p.parsed, &movie_exp) {
                    Some((idx, _)) => {
                        let mt = self.library.importer().movie_target(movies[idx].id).await?;
                        if self.is_sample(p, mt.movie.runtime_minutes) {
                            p.rejections.push("Sample".into());
                        }
                        p.target = Some(Target::Movie(mt.movie.id));
                    }
                    None => p.rejections.push(if p.parsed.title.is_empty() {
                        "Unable to parse the file name".to_owned()
                    } else {
                        format!("Unknown movie: {}", p.parsed.title)
                    }),
                }
            }
        }
        for (sid, idxs) in by_series {
            let st = self.library.importer().series_target(sid).await?;
            let mut subset: Vec<PlannedFile> = idxs.iter().map(|i| planned[*i].clone()).collect();
            self.map_series(&mut subset, &st, &[]);
            for (i, p) in idxs.into_iter().zip(subset) {
                planned[i] = p;
            }
        }
        Ok(())
    }

    /// Fills score, destination, replaced files and the remaining rejections (upgrade, already
    /// imported, free space) for a mapped file.
    pub async fn evaluate(&self, p: &mut PlannedFile) -> Result<()> {
        let Some(target) = p.target.clone() else {
            if p.rejections.is_empty() {
                p.rejections.push("Unknown movie or series".into());
            }
            return Ok(());
        };
        let imp = self.library.importer();
        let ext = extension(&p.path).unwrap_or_else(|| "mkv".into());
        let original = p.path.file_stem().map(|s| s.to_string_lossy().into_owned());
        let (profile, existing, dest) = match &target {
            Target::Movie(id) => {
                let mt = imp.movie_target(*id).await?;
                p.score = imp.score(&p.parsed, p.size, mt.movie.quality_profile_id).await?;
                let dest = imp
                    .movie_destination(&mt.movie, &naming_input(p, original.as_deref(), &ext))
                    .await;
                (mt.profile, mt.existing.into_iter().collect::<Vec<_>>(), dest)
            }
            Target::Episodes {
                series_id,
                episode_ids,
            } => {
                let st = imp.series_target(*series_id).await?;
                p.score = imp.score(&p.parsed, p.size, st.series.quality_profile_id).await?;
                let eps: Vec<_> = st
                    .episodes
                    .iter()
                    .filter(|e| episode_ids.contains(&e.id))
                    .collect();
                if eps.len() != episode_ids.len() {
                    p.rejections.push("Episodes are not part of the series".into());
                    return Ok(());
                }
                let dest = imp
                    .episode_destination(&st.series, &eps, &naming_input(p, original.as_deref(), &ext))
                    .await;
                let existing: Vec<ExistingFile> = st.files_for(episode_ids).into_iter().cloned().collect();
                (st.profile, existing, dest)
            }
        };
        match dest {
            Ok(d) => p.destination = Some(d),
            Err(e) => p
                .rejections
                .push(format!("Cannot build the destination path: {e}")),
        }
        let candidate = Scored {
            quality: p.quality,
            format_score: p.score.score,
        };
        for e in &existing {
            if e.original_path.as_deref() == Some(&*p.path.to_string_lossy()) || e.path == p.path {
                p.rejections.push("Already imported".into());
                continue;
            }
            let old = Scored {
                quality: e.quality,
                format_score: e.format_score,
            };
            if let Some(profile) = &profile
                && !reelix_decision::is_upgrade(&old, &candidate, profile)
            {
                p.rejections.push(format!(
                    "Not an upgrade for the existing file ({}, score {})",
                    e.quality.quality.name(),
                    e.format_score
                ));
            }
        }
        p.is_upgrade = !existing.is_empty();
        p.replaces = existing.iter().map(|e| e.path.clone()).collect();
        if !self.settings.skip_free_space_check
            && let Some(d) = &p.destination
            && let Ok(space) = disk_space(d)
        {
            let need = p.size + self.settings.minimum_free_space_mb * MIB;
            if space.available < need {
                p.rejections.push(format!(
                    "Not enough free space: {} MiB available, {} MiB needed (incl. minimum free space)",
                    space.available / MIB,
                    need / MIB
                ));
            }
        }
        Ok(())
    }

    /// Transfers one accepted file into the library and records it.
    /// `roots`: download folders the source may live in (added to the path guard).
    pub async fn execute(
        &self,
        p: &PlannedFile,
        mode: TransferMode,
        roots: &[PathBuf],
    ) -> Result<ImportedItem> {
        let target = p
            .target
            .clone()
            .ok_or_else(|| ActivityError::validation("file is not mapped to a movie or series"))?;
        let dest = p
            .destination
            .clone()
            .ok_or_else(|| ActivityError::validation("no destination for the file"))?;
        let imp = self.library.importer();
        let mut guard_roots = roots.to_vec();
        if let Some(parent) = p.path.parent() {
            guard_roots.push(parent.to_path_buf());
        }
        let guard = imp.guard(&guard_roots).await?;
        guard.check_existing(&p.path)?;
        guard.check(&dest)?;

        let mut replaced = Vec::new();
        for old in &p.replaces {
            if old == &p.path || !old.exists() {
                continue;
            }
            match imp.remove_file(old).await {
                Ok(bin) => replaced.push(bin.unwrap_or_else(|| old.clone())),
                Err(e) => tracing::warn!(file = %old.display(), error = %e, "could not remove replaced file"),
            }
        }
        let opts = TransferOptions {
            overwrite: true,
            permissions: self.permissions(),
            preserve_mtime: true,
        };
        let (src, dst) = (p.path.clone(), dest.clone());
        let outcome = tokio::task::spawn_blocking(move || transfer_with(&src, &dst, mode, &opts))
            .await
            .map_err(|e| ActivityError::Other(e.to_string()))??;
        tracing::info!(from = %p.path.display(), to = %dest.display(), method = ?outcome.method, "imported");

        let file = ImportedFile {
            path: dest.clone(),
            size: p.size,
            quality: p.quality,
            languages: p.languages.clone(),
            release_group: p.release_group.clone(),
            edition: p.parsed.edition.clone(),
            scene_name: p.scene_name.clone(),
            original_path: Some(p.path.to_string_lossy().into_owned()),
            media_info: p.media_info.clone(),
            score: p.score.clone(),
        };
        let file_id = match &target {
            Target::Movie(id) => imp.record_movie_file(*id, &file).await?,
            Target::Episodes {
                series_id,
                episode_ids,
            } => imp.record_episode_file(*series_id, episode_ids, &file).await?,
        };

        let mut extras = Vec::new();
        if self.settings.import_extra_files {
            let stem = dest
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            for (src, suffix) in &p.extras {
                let Some(parent) = dest.parent() else { continue };
                let target_path = parent.join(format!("{stem}{suffix}"));
                if guard.check(&target_path).is_err() {
                    continue;
                }
                let (s, d) = (src.clone(), target_path.clone());
                let extra_mode = if mode == TransferMode::Move {
                    TransferMode::Move
                } else {
                    TransferMode::Copy
                };
                match tokio::task::spawn_blocking(move || transfer_with(&s, &d, extra_mode, &opts)).await {
                    Ok(Ok(_)) => extras.push(target_path),
                    Ok(Err(e)) => {
                        tracing::warn!(file = %src.display(), error = %e, "extra file not imported")
                    }
                    Err(e) => tracing::warn!(error = %e, "extra file task failed"),
                }
            }
        }
        Ok(ImportedItem {
            target,
            file_id,
            source: p.path.clone(),
            destination: dest,
            size: p.size,
            quality: p.quality,
            languages: p.languages.clone(),
            release_group: p.release_group.clone(),
            score: p.score.clone(),
            is_upgrade: p.is_upgrade,
            replaced,
            extras,
            scene_name: p.scene_name.clone(),
        })
    }
}

fn naming_input<'a>(p: &'a PlannedFile, original: Option<&'a str>, ext: &'a str) -> NamingInput<'a> {
    NamingInput {
        parsed: &p.parsed,
        media_info: p.media_info.as_ref(),
        quality: p.quality,
        score: &p.score,
        original_file_name: original,
        ext,
    }
}

/// Extra files for `video`: files named `<video stem>.*` anywhere in the scan, or — when the
/// download holds a single video — subtitles in the same folder or a `Subs` folder.
fn extras_for(
    video: &Path,
    others: &[FoundFile],
    single: bool,
    settings: &MediaManagementSettings,
) -> Vec<(PathBuf, String)> {
    let allowed: Vec<String> = settings
        .extra_file_extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .collect();
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = video.parent();
    let mut out = Vec::new();
    for o in others {
        let Some(ext) = extension(&o.path) else { continue };
        if !allowed.contains(&ext) || o.extra.is_some() {
            continue;
        }
        let name = o
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !stem.is_empty() && name.starts_with(&stem) && name.len() > stem.len() {
            out.push((o.path.clone(), name[stem.len()..].to_owned()));
            continue;
        }
        if !single || o.kind != FileKind::Subtitle {
            continue;
        }
        let parent = o.path.parent();
        let in_subs = parent
            .and_then(Path::file_name)
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("subs"));
        if parent == dir || in_subs {
            let ostem = o
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push((o.path.clone(), format!(".{ostem}.{ext}")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(
            transfer_mode(ImportMode::HardlinkOrCopy, true, false),
            TransferMode::Move
        );
        assert_eq!(transfer_mode(ImportMode::Copy, true, true), TransferMode::Copy);
        assert_eq!(
            transfer_mode(ImportMode::HardlinkOrCopy, false, true),
            TransferMode::HardlinkOrCopy
        );
        assert_eq!(
            transfer_mode(ImportMode::Move, false, false),
            TransferMode::HardlinkOrCopy
        );
        assert_eq!(transfer_mode(ImportMode::Move, false, true), TransferMode::Move);
        assert_eq!(parse_mode("auto"), Some(None));
        assert_eq!(parse_mode("COPY"), Some(Some(TransferMode::Copy)));
        assert_eq!(parse_mode("x"), None);
    }

    #[test]
    fn extra_suffixes() {
        let f = |p: &str, kind| FoundFile {
            path: PathBuf::from(p),
            relative: PathBuf::from(p),
            size: 1,
            kind,
            extra: None,
            modified: None,
        };
        let others = vec![
            f("/d/Movie.2020.1080p/Movie.2020.1080p.en.srt", FileKind::Subtitle),
            f("/d/Movie.2020.1080p/Subs/English.srt", FileKind::Subtitle),
            f("/d/Movie.2020.1080p/readme.txt", FileKind::Other),
        ];
        let got = extras_for(
            Path::new("/d/Movie.2020.1080p/Movie.2020.1080p.mkv"),
            &others,
            true,
            &MediaManagementSettings::default(),
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1, ".en.srt");
        assert_eq!(got[1].1, ".English.srt");
    }
}
