//! Archives in finished downloads: find the first volume of each RAR, ZIP or 7z set, list it and
//! unpack it (`unrar` for RAR, the `zip` crate for ZIP, 7-Zip for 7z).
//!
//! Files are unpacked into a hidden staging folder next to the archive ([`STAGING_DIR`]) and moved
//! into the archive's folder only when unpacking succeeds, so a half-unpacked file is never seen
//! by an import (scans skip folders whose names start with `.`). Programs and scripts
//! ([`crate::files::is_dangerous`]) are never unpacked. The archives themselves are left alone
//! for the download client to seed and remove; an archive found inside another one (scene book
//! releases: a `.zip` holding a `.rar` holding the `.epub`) is unpacked in turn, and that copy is
//! one of the unpacked files, removed after import like the others.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::files::{DANGEROUS_EXTENSIONS, is_dangerous, is_media};

/// Hidden folder, next to the archive, that archives are unpacked into.
pub const STAGING_DIR: &str = ".reelix-unpack";

/// How deep below the download folder archives are looked for (season packs keep one set per
/// episode folder).
const MAX_DEPTH: usize = 3;

/// How many times archives found inside unpacked archives are unpacked in turn.
const MAX_ROUNDS: usize = 3;

/// The kinds of archive Reelix unpacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Rar,
    Zip,
    SevenZip,
}

/// How to run the unpacking programs.
#[derive(Debug, Clone)]
pub struct UnpackConfig {
    /// `unrar` binary (default `unrar` from PATH).
    pub unrar_path: PathBuf,
    /// 7-Zip binaries to try in order (default `7zz`, `7z`, then `7za` from PATH).
    pub seven_zip_paths: Vec<PathBuf>,
    /// Limit for one archive set, after which the program is stopped.
    pub timeout: Duration,
}

impl Default for UnpackConfig {
    fn default() -> Self {
        Self {
            unrar_path: PathBuf::from("unrar"),
            seven_zip_paths: ["7zz", "7z", "7za"].iter().map(PathBuf::from).collect(),
            timeout: Duration::from_secs(2 * 60 * 60),
        }
    }
}

/// Why an archive could not be listed or unpacked. The messages are shown in the queue.
#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    #[error("unrar is not installed, so the RAR archives cannot be unpacked")]
    NotInstalled,
    #[error("7-Zip is not installed, so the 7z archives cannot be unpacked")]
    SevenZipNotInstalled,
    #[error("the archive is password protected")]
    Encrypted,
    #[error("the archive is damaged or incomplete")]
    Damaged,
    #[error("unpacking stopped after {0:?}")]
    Timeout(Duration),
    #[error("unpacking failed (exit {code:?}): {stderr}")]
    Failed { code: Option<i32>, stderr: String },
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl UnpackError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// The program is missing: no other archive of that kind can be unpacked either.
    fn is_missing_program(&self) -> bool {
        matches!(self, Self::NotInstalled | Self::SevenZipNotInstalled)
    }
}

/// Whether `path` is the volume `unrar` should be given: a `.rar` file that is not a later part
/// of a `.partN.rar` set. Old-style sets (`.rar`, `.r00`, `.r01` and so on) start at the `.rar`.
pub fn is_first_volume(path: &Path) -> bool {
    archive_kind(path) == Some(ArchiveKind::Rar)
}

/// The kind of archive `path` is, when it is the first (or only) volume of its set: `x.rar` or
/// `x.part01.rar`, `x.zip`, `x.7z` or `x.7z.001`. Later volumes give `None`.
pub fn archive_kind(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if let Some(stem) = name.strip_suffix(".rar") {
        return match stem.rsplit_once(".part") {
            Some((_, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                (n.trim_start_matches('0') == "1").then_some(ArchiveKind::Rar)
            }
            _ => Some(ArchiveKind::Rar),
        };
    }
    if name.ends_with(".zip") {
        return Some(ArchiveKind::Zip);
    }
    if name.ends_with(".7z") || name.ends_with(".7z.001") {
        return Some(ArchiveKind::SevenZip);
    }
    None
}

/// First volumes of the archive sets below `dir` (or `dir` itself when it is an archive),
/// sorted. Hidden folders and the `_UNPACK_` / `_FAILED_` folders of Usenet clients are skipped.
pub fn find_archives(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if depth < MAX_DEPTH
                    && !name.starts_with('.')
                    && !name.starts_with("_unpack")
                    && !name.starts_with("_failed")
                {
                    walk(&path, depth + 1, out);
                }
            } else if ft.is_file() && archive_kind(&path).is_some() {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    if dir.is_file() {
        if archive_kind(dir).is_some() {
            out.push(dir.to_path_buf());
        }
        return out;
    }
    walk(dir, 0, &mut out);
    out.sort();
    out
}

/// Paths inside `archive` (relative; folders may be included).
pub async fn list(archive: &Path, config: &UnpackConfig) -> Result<Vec<PathBuf>, UnpackError> {
    match archive_kind(archive) {
        Some(ArchiveKind::Zip) => {
            let a = archive.to_path_buf();
            tokio::task::spawn_blocking(move || zip_list(&a))
                .await
                .unwrap_or_else(|e| Err(UnpackError::io(archive, std::io::Error::other(e))))
        }
        Some(ArchiveKind::SevenZip) => {
            let out = run_7z(config, &["l", "-slt", "-ba", "-p"], archive).await?;
            Ok(String::from_utf8_lossy(&out)
                .lines()
                .filter_map(|l| l.strip_prefix("Path = "))
                .map(|p| PathBuf::from(p.trim()))
                .collect())
        }
        _ => {
            let out = run_unrar(config, &["lb", "-p-", "--"], archive, None).await?;
            Ok(String::from_utf8_lossy(&out)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect())
        }
    }
}

/// Whether `archive` holds at least one video file.
pub async fn has_video(archive: &Path, config: &UnpackConfig) -> Result<bool, UnpackError> {
    Ok(list(archive, config).await?.iter().any(|p| crate::files::is_video(p)))
}

/// Whether `archive` holds a media file (video, audio, book) or another archive.
pub async fn worth_unpacking(archive: &Path, config: &UnpackConfig) -> Result<bool, UnpackError> {
    Ok(list(archive, config)
        .await?
        .iter()
        .any(|p| is_media(p) || archive_kind(p).is_some()))
}

/// Unpacks `archive` (all of its volumes) into the folder it is in. Returns the files that were
/// added; a file that already exists there is kept and not replaced, and programs are left out.
pub async fn unpack(archive: &Path, config: &UnpackConfig) -> Result<Vec<PathBuf>, UnpackError> {
    let dir = archive
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stage = dir.join(STAGING_DIR);
    // A staging folder left by an earlier run (Reelix stopped part-way) holds nothing usable.
    remove_staging(&stage).await?;
    tokio::fs::create_dir_all(&stage)
        .await
        .map_err(|e| UnpackError::io(&stage, e))?;
    let result = match archive_kind(archive) {
        Some(ArchiveKind::Zip) => {
            let (a, s) = (archive.to_path_buf(), stage.clone());
            tokio::task::spawn_blocking(move || zip_extract(&a, &s))
                .await
                .unwrap_or_else(|e| Err(UnpackError::io(archive, std::io::Error::other(e))))
        }
        Some(ArchiveKind::SevenZip) => {
            let mut args: Vec<String> = ["x", "-y", "-p", "-aos"].map(String::from).to_vec();
            args.push(format!("-o{}", stage.display()));
            args.extend(DANGEROUS_EXTENSIONS.iter().map(|e| format!("-xr!*.{e}")));
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            run_7z(config, &args, archive).await.map(|_| ())
        }
        _ => {
            let mut args: Vec<String> = ["x", "-o+", "-p-", "-y", "-idq"].map(String::from).to_vec();
            args.extend(DANGEROUS_EXTENSIONS.iter().map(|e| format!("-x*.{e}")));
            args.push("--".into());
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            run_unrar(config, &args, archive, Some(&stage)).await.map(|_| ())
        }
    };
    let moved = match result {
        Ok(()) => {
            let (stage2, dir2) = (stage.clone(), dir.clone());
            tokio::task::spawn_blocking(move || move_out(&stage2, &dir2))
                .await
                .unwrap_or_else(|e| Err(UnpackError::io(&stage, std::io::Error::other(e))))
        }
        Err(e) => Err(e),
    };
    remove_staging(&stage).await?;
    moved
}

/// What [`unpack_download`] did: the files it added and why any archive was not unpacked.
#[derive(Debug, Default)]
pub struct Unpacked {
    pub files: Vec<PathBuf>,
    pub messages: Vec<String>,
}

/// Unpacks every archive below `dir` that holds media or another archive, then any archive
/// that unpacking brought out. Archives without media (subtitles, covers) are left packed.
/// Each archive that cannot be unpacked gets a message naming it.
pub async fn unpack_download(dir: &Path, config: &UnpackConfig) -> Unpacked {
    let base = if dir.is_file() {
        dir.parent().unwrap_or(dir).to_path_buf()
    } else {
        dir.to_path_buf()
    };
    let mut done = Unpacked::default();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut without_media = 0;
    let mut missing: Vec<ArchiveKind> = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let root = dir.to_path_buf();
        let archives: Vec<PathBuf> = tokio::task::spawn_blocking(move || find_archives(&root))
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|a| !seen.contains(a))
            .collect();
        if archives.is_empty() {
            break;
        }
        for archive in archives {
            seen.push(archive.clone());
            let kind = archive_kind(&archive).unwrap_or(ArchiveKind::Rar);
            if missing.contains(&kind) {
                continue;
            }
            let name = archive.strip_prefix(&base).unwrap_or(&archive).display().to_string();
            let result = match worth_unpacking(&archive, config).await {
                Ok(false) => {
                    without_media += 1;
                    continue;
                }
                Ok(true) => unpack(&archive, config).await,
                Err(e) => Err(e),
            };
            match result {
                Ok(files) => {
                    tracing::info!(archive = %archive.display(), files = files.len(), "unpacked");
                    done.files.extend(files);
                }
                Err(e) if e.is_missing_program() => {
                    missing.push(kind);
                    done.messages.push(e.to_string());
                }
                Err(e) => {
                    tracing::warn!(archive = %archive.display(), error = %e, "could not unpack");
                    done.messages.push(format!("{name}: {e}"));
                }
            }
        }
    }
    if done.files.is_empty() && done.messages.is_empty() && without_media > 0 {
        done.messages
            .push("The archives in the download hold no video, audio or book files".into());
    }
    done
}

/// Whether `source` (a folder or a file) holds a media file, or an archive that holds one.
/// Tells a download that is only a program passed off as media from one with a stray program
/// beside real media. An archive that cannot be read counts as media, so a download is never
/// thrown away because of an archive Reelix cannot look into.
pub async fn holds_media(source: &Path, config: &UnpackConfig) -> bool {
    let root = source.to_path_buf();
    let files = tokio::task::spawn_blocking(move || {
        let opts = crate::files::ScanOptions {
            include_non_video: true,
            ..Default::default()
        };
        crate::files::scan(&root, &opts).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    if files.iter().any(|f| is_media(&f.path)) {
        return true;
    }
    for f in files.iter().filter(|f| archive_kind(&f.path).is_some()) {
        if worth_unpacking(&f.path, config).await.unwrap_or(true) {
            return true;
        }
    }
    false
}

/// Removes files that [`unpack_download`] added, once they have been imported. Files that are
/// gone already (moved by the import) are skipped.
pub async fn remove_unpacked(files: &[PathBuf]) {
    for f in files {
        match tokio::fs::remove_file(f).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(path = %f.display(), error = %e, "could not remove an unpacked file"),
        }
    }
}

async fn remove_staging(stage: &Path) -> Result<(), UnpackError> {
    match tokio::fs::remove_dir_all(stage).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(UnpackError::io(stage, e)),
    }
}

/// Moves every file below `stage` to the same place below `dir` (one file system, so a rename).
/// Programs are not moved; they go with the staging folder.
fn move_out(stage: &Path, dir: &Path) -> Result<Vec<PathBuf>, UnpackError> {
    fn walk(from: &Path, to: &Path, out: &mut Vec<PathBuf>) -> Result<(), UnpackError> {
        let entries = std::fs::read_dir(from).map_err(|e| UnpackError::io(from, e))?;
        for e in entries {
            let e = e.map_err(|e| UnpackError::io(from, e))?;
            let src = e.path();
            let dst = to.join(e.file_name());
            let ft = e.file_type().map_err(|e| UnpackError::io(&src, e))?;
            if ft.is_dir() {
                std::fs::create_dir_all(&dst).map_err(|e| UnpackError::io(&dst, e))?;
                walk(&src, &dst, out)?;
            } else if ft.is_file() && !dst.exists() && !is_dangerous(&src) {
                std::fs::rename(&src, &dst).map_err(|e| UnpackError::io(&dst, e))?;
                out.push(dst);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(stage, dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn zip_error(archive: &Path, e: zip::result::ZipError) -> UnpackError {
    use zip::result::ZipError;
    match e {
        ZipError::Io(io) => UnpackError::io(archive, io),
        ZipError::InvalidPassword => UnpackError::Encrypted,
        ZipError::UnsupportedArchive(msg) if msg.to_ascii_lowercase().contains("password") => {
            UnpackError::Encrypted
        }
        ZipError::UnsupportedArchive(msg) => UnpackError::Failed {
            code: None,
            stderr: msg.to_string(),
        },
        _ => UnpackError::Damaged,
    }
}

fn zip_open(archive: &Path) -> Result<zip::ZipArchive<std::fs::File>, UnpackError> {
    let file = std::fs::File::open(archive).map_err(|e| UnpackError::io(archive, e))?;
    zip::ZipArchive::new(file).map_err(|e| zip_error(archive, e))
}

fn zip_list(archive: &Path) -> Result<Vec<PathBuf>, UnpackError> {
    let zip = zip_open(archive)?;
    Ok(zip
        .file_names()
        .filter_map(Result::ok)
        .map(|n| PathBuf::from(n.as_ref()))
        .collect())
}

/// Extracts `archive` into `stage`: files only, under safe relative names, programs left out.
fn zip_extract(archive: &Path, stage: &Path) -> Result<(), UnpackError> {
    let mut zip = zip_open(archive)?;
    for i in 0..zip.len() {
        if zip.by_index_raw(i).map_err(|e| zip_error(archive, e))?.encrypted() {
            return Err(UnpackError::Encrypted);
        }
        let mut entry = zip.by_index(i).map_err(|e| zip_error(archive, e))?;
        // A name that would leave the staging folder (`../`, absolute) is skipped.
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        if entry.is_dir() || entry.is_symlink() || is_dangerous(&name) {
            continue;
        }
        let dest = stage.join(&name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| UnpackError::io(parent, e))?;
        }
        let mut out = std::fs::File::create(&dest).map_err(|e| UnpackError::io(&dest, e))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| match e.kind() {
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => UnpackError::Damaged,
            _ => UnpackError::io(&dest, e),
        })?;
    }
    Ok(())
}

/// Runs `program args archive [dest/]`. `None` when the program is not installed.
async fn output(
    program: &Path,
    args: &[&str],
    archive: &Path,
    dest: Option<&Path>,
    timeout: Duration,
) -> Result<Option<std::process::Output>, UnpackError> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).arg(archive);
    if let Some(d) = dest {
        // The trailing separator tells unrar that this is the destination folder.
        let mut d = d.as_os_str().to_owned();
        d.push(std::path::MAIN_SEPARATOR_STR);
        cmd.arg(d);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(UnpackError::io(program, e)),
    };
    tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| UnpackError::Timeout(timeout))?
        .map(Some)
        .map_err(|e| UnpackError::io(program, e))
}

async fn run_unrar(
    config: &UnpackConfig,
    args: &[&str],
    archive: &Path,
    dest: Option<&Path>,
) -> Result<Vec<u8>, UnpackError> {
    let output = output(&config.unrar_path, args, archive, dest, config.timeout)
        .await?
        .ok_or(UnpackError::NotInstalled)?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    match output.status.code() {
        // 1 is a warning that still unpacked everything.
        Some(0 | 1) => Ok(output.stdout),
        Some(11) => Err(UnpackError::Encrypted),
        _ if stderr.to_ascii_lowercase().contains("password") => Err(UnpackError::Encrypted),
        // 3: checksum error; 2: fatal error; 10: missing volume or nothing to unpack. unrar 7 reports a
        // volume it cannot find as 6 (open error), with "Cannot find volume" on stderr.
        Some(2 | 3 | 10) => Err(UnpackError::Damaged),
        Some(6) if stderr.to_ascii_lowercase().contains("cannot find volume") => Err(UnpackError::Damaged),
        code => Err(UnpackError::Failed { code, stderr }),
    }
}

async fn run_7z(config: &UnpackConfig, args: &[&str], archive: &Path) -> Result<Vec<u8>, UnpackError> {
    for program in &config.seven_zip_paths {
        let Some(output) = output(program, args, archive, None, config.timeout).await? else {
            continue;
        };
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let all = format!("{stderr}\n{}", String::from_utf8_lossy(&output.stdout)).to_ascii_lowercase();
        return match output.status.code() {
            // 1 is a warning (a file in use, for example) after which the rest was unpacked.
            Some(0 | 1) => Ok(output.stdout),
            _ if all.contains("wrong password") || all.contains("encrypted") => Err(UnpackError::Encrypted),
            // 2: fatal error (a damaged archive or a missing volume).
            Some(2) => Err(UnpackError::Damaged),
            code => Err(UnpackError::Failed { code, stderr }),
        };
    }
    Err(UnpackError::SevenZipNotInstalled)
}
