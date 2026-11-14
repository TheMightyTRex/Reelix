//! Filesystem side of imports: discovery, classification, path safety and transfers.

mod guard;
mod recycle;
mod space;
mod transfer;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use regex::Regex;
use serde::{Deserialize, Serialize};

pub use guard::PathGuard;
pub use recycle::{CleanupReport, RecycleBin, delete_permanently};
pub use space::{DiskSpace, disk_space, same_filesystem};
pub use transfer::{
    PermissionSettings, TransferMethod, TransferMode, TransferOptions, TransferOutcome, apply_permissions,
    transfer, transfer_with,
};

use crate::{Error, Result};

/// Video container extensions (lowercase, without dot).
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "wmv", "mpg", "mpeg", "m2ts", "mts", "ts", "webm", "flv", "ogm",
    "ogv", "divx", "xvid", "vob", "3gp", "asf", "rmvb", "f4v", "strm",
];
/// Subtitle sidecar extensions.
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "sup", "vtt", "smi"];
/// Metadata sidecar extensions.
pub const METADATA_EXTENSIONS: &[&str] = &["nfo", "xml"];
/// Image sidecar extensions (posters, fanart).
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "tbn"];
/// Programs and scripts (lowercase, without dot). A film, episode, book or album never
/// needs one, so a download holding any is held instead of imported: it is most likely
/// malware passed off as media.
pub const DANGEROUS_EXTENSIONS: &[&str] = &[
    "exe", "scr", "com", "pif", "cpl", "msi", "msp", "bat", "cmd", "lnk", "js", "jse", "vbs", "vbe", "wsf",
    "wsh", "hta", "ps1", "psm1", "jar", "reg", "dll",
];
/// Download client in-progress markers; such files are never imported.
pub const PARTIAL_EXTENSIONS: &[&str] = &["part", "partial", "!qb", "!ut", "crdownload", "tmp", "dlpart"];

/// Directory names skipped by [`scan`] (compared case-insensitively). Names starting
/// with `.` are skipped as well.
pub const SKIPPED_DIRS: &[&str] = &[
    "@eadir",
    "#recycle",
    "@recycle",
    "#snapshot",
    "$recycle.bin",
    "system volume information",
    "lost+found",
    "@__thumb",
    ".grab",
    "_unpack",
    "_failed",
    "extrathumbs",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Video,
    Subtitle,
    Metadata,
    Image,
    Other,
}

/// Plex/Jellyfin-style extra categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraKind {
    Trailer,
    Featurette,
    BehindTheScenes,
    DeletedScene,
    Interview,
    Scene,
    Short,
    Other,
}

/// Lowercased extension of `path` (without dot).
pub fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

pub fn is_video(path: &Path) -> bool {
    extension(path).is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.as_str()))
}

/// True for a program or script ([`DANGEROUS_EXTENSIONS`]), including a disguised one
/// such as `Film.mkv.exe`.
pub fn is_dangerous(path: &Path) -> bool {
    extension(path).is_some_and(|e| DANGEROUS_EXTENSIONS.contains(&e.as_str()))
}

/// The programs and scripts below `source` (a folder or a single file), as paths relative to
/// it, sorted. Unreadable folders are skipped.
pub async fn dangerous_files(source: &Path) -> Vec<PathBuf> {
    let opts = ScanOptions {
        include_non_video: true,
        ..ScanOptions::default()
    };
    scan_async(source.to_path_buf(), opts)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|f| is_dangerous(&f.path))
        .map(|f| f.relative)
        .collect()
}

pub fn classify(path: &Path) -> FileKind {
    let Some(ext) = extension(path) else {
        return FileKind::Other;
    };
    let ext = ext.as_str();
    if VIDEO_EXTENSIONS.contains(&ext) {
        FileKind::Video
    } else if SUBTITLE_EXTENSIONS.contains(&ext) {
        FileKind::Subtitle
    } else if METADATA_EXTENSIONS.contains(&ext) {
        FileKind::Metadata
    } else if IMAGE_EXTENSIONS.contains(&ext) {
        FileKind::Image
    } else {
        FileKind::Other
    }
}

static EXTRA_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"-(trailer|featurette|behindthescenes|deleted|deletedscene|interview|scene|short|other|extra)$",
    )
    .expect("extra suffix regex")
});

fn extra_from_word(word: &str) -> Option<ExtraKind> {
    let w: String = word
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match w.as_str() {
        "trailer" | "trailers" => ExtraKind::Trailer,
        "featurette" | "featurettes" => ExtraKind::Featurette,
        "behindthescenes" => ExtraKind::BehindTheScenes,
        "deleted" | "deletedscene" | "deletedscenes" => ExtraKind::DeletedScene,
        "interview" | "interviews" => ExtraKind::Interview,
        "scene" | "scenes" => ExtraKind::Scene,
        "short" | "shorts" => ExtraKind::Short,
        "other" | "others" | "extra" | "extras" | "bonus" | "specialfeatures" => ExtraKind::Other,
        _ => return None,
    })
}

/// Detect extras by a lowercase `-trailer`-style file name suffix (Plex convention; case
/// sensitive so release groups like `-SCENE` don't match) or by a parent folder such as
/// `Featurettes/` or `Behind The Scenes/` (below `relative_to`, when given).
pub fn detect_extra(path: &Path, relative_to: Option<&Path>) -> Option<ExtraKind> {
    let stem = path.file_stem()?.to_string_lossy();
    if let Some(c) = EXTRA_SUFFIX.captures(&stem)
        && let Some(kind) = extra_from_word(&c[1])
    {
        return Some(kind);
    }
    let rel = relative_to
        .and_then(|r| path.strip_prefix(r).ok())
        .unwrap_or(path);
    let parent = rel.parent()?;
    parent
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_string_lossy().to_string()),
            _ => None,
        })
        .find_map(|n| extra_from_word(&n))
}

static SAMPLE_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|[-_. \[(])sample(?:$|[-_. \])])").expect("sample regex"));

/// Why a file was judged to be a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleReason {
    /// `sample` in the file name or its folder name.
    Name,
    /// Much shorter than the expected runtime.
    Runtime,
    /// Too small for the expected runtime (used when the runtime is unknown).
    Size,
}

/// Thresholds for [`detect_sample`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SampleRules {
    /// Files shorter than this fraction of the expected runtime are samples…
    pub runtime_fraction: f64,
    /// …capped at this many seconds (so long movies don't need 30-minute minimums).
    pub max_runtime_threshold_secs: u64,
    /// Size floor per expected runtime minute when the actual runtime is unknown.
    pub min_bytes_per_minute: u64,
    /// Absolute size floor cap.
    pub max_size_threshold_bytes: u64,
}

impl Default for SampleRules {
    fn default() -> Self {
        Self {
            runtime_fraction: 0.2,
            max_runtime_threshold_secs: 600,
            min_bytes_per_minute: 3 * 1024 * 1024,
            max_size_threshold_bytes: 70 * 1024 * 1024,
        }
    }
}

/// Decide whether a video is a sample. `runtime` is the probed duration,
/// `expected_runtime_minutes` the metadata runtime of the movie/episode.
pub fn detect_sample(
    path: &Path,
    size_bytes: u64,
    runtime: Option<Duration>,
    expected_runtime_minutes: Option<u32>,
    rules: &SampleRules,
) -> Option<SampleReason> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|p| p.to_string_lossy().to_ascii_lowercase());
    if SAMPLE_WORD.is_match(&stem) || matches!(parent.as_deref(), Some("sample" | "samples")) {
        return Some(SampleReason::Name);
    }
    let expected = expected_runtime_minutes.filter(|m| *m > 0)?;
    match runtime {
        Some(rt) if rt > Duration::ZERO => {
            let threshold = (f64::from(expected) * 60.0 * rules.runtime_fraction)
                .min(rules.max_runtime_threshold_secs as f64);
            (rt.as_secs_f64() < threshold).then_some(SampleReason::Runtime)
        }
        _ => {
            let threshold =
                (u64::from(expected) * rules.min_bytes_per_minute).min(rules.max_size_threshold_bytes);
            (size_bytes < threshold).then_some(SampleReason::Size)
        }
    }
}

/// A file discovered by [`scan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundFile {
    pub path: PathBuf,
    /// Path relative to the scanned directory.
    pub relative: PathBuf,
    pub size: u64,
    pub kind: FileKind,
    pub extra: Option<ExtraKind>,
    pub modified: Option<SystemTime>,
}

/// What [`scan`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Include subtitle/metadata/image/other files, not only videos.
    pub include_non_video: bool,
    /// Maximum directory depth below the scanned directory.
    pub max_depth: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            include_non_video: false,
            max_depth: 16,
        }
    }
}

fn skip_name(name: &str) -> bool {
    name.starts_with('.') || SKIPPED_DIRS.contains(&name.to_ascii_lowercase().as_str())
}

/// Recursively list files below `dir` (sorted by path). Hidden entries, NAS metadata
/// folders (`@eaDir`, `#recycle`…), partial downloads and symlinked directories are
/// skipped. `dir` may also be a single file.
pub fn scan(dir: &Path, opts: &ScanOptions) -> Result<Vec<FoundFile>> {
    let meta = std::fs::metadata(dir).map_err(|e| Error::io(dir, e))?;
    let mut out = Vec::new();
    if meta.is_file() {
        let base = dir.parent().unwrap_or(Path::new(""));
        push_file(dir, base, &meta, opts, &mut out);
        return Ok(out);
    }
    walk(dir, dir, 0, opts, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// [`scan`] on the blocking thread pool.
pub async fn scan_async(dir: PathBuf, opts: ScanOptions) -> Result<Vec<FoundFile>> {
    tokio::task::spawn_blocking(move || scan(&dir, &opts))
        .await
        .map_err(|e| Error::io(PathBuf::new(), std::io::Error::other(e)))?
}

fn walk(root: &Path, dir: &Path, depth: usize, opts: &ScanOptions, out: &mut Vec<FoundFile>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if skip_name(&name) {
            continue;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if depth < opts.max_depth {
                walk(root, &path, depth + 1, opts, out)?;
            }
        } else if ft.is_file() || ft.is_symlink() {
            // Symlinked files are listed (they may be legit); symlinked dirs are not followed.
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_file() {
                push_file(&path, root, &meta, opts, out);
            }
        }
    }
    Ok(())
}

fn push_file(
    path: &Path,
    root: &Path,
    meta: &std::fs::Metadata,
    opts: &ScanOptions,
    out: &mut Vec<FoundFile>,
) {
    if extension(path).is_some_and(|e| PARTIAL_EXTENSIONS.contains(&e.as_str())) {
        return;
    }
    let kind = classify(path);
    if kind != FileKind::Video && !opts.include_non_video {
        return;
    }
    out.push(FoundFile {
        path: path.to_path_buf(),
        relative: path.strip_prefix(root).unwrap_or(path).to_path_buf(),
        size: meta.len(),
        kind,
        extra: if kind == FileKind::Video {
            detect_extra(path, Some(root))
        } else {
            None
        },
        modified: meta.modified().ok(),
    });
}
