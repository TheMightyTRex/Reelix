//! Recycle bin: deleted/replaced files are moved into timestamped folders and purged
//! after a retention period instead of being deleted outright.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use super::PathGuard;
use super::transfer::{TransferMode, TransferOptions, transfer_with};
use crate::{Error, Result};

/// Timestamp folder format (no `:` so it is valid on Windows).
const STAMP_FORMAT: &str = "%Y-%m-%d_%H-%M-%S";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecycleBin {
    path: PathBuf,
    /// `None` keeps items forever.
    retention: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CleanupReport {
    pub removed: usize,
    pub kept: usize,
}

impl RecycleBin {
    /// Use `path` as the bin (created when missing).
    pub fn new(path: impl Into<PathBuf>, retention: Option<Duration>) -> Result<Self> {
        let path = path.into();
        std::fs::create_dir_all(&path).map_err(|e| Error::io(&path, e))?;
        Ok(Self { path, retention })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn retention(&self) -> Option<Duration> {
        self.retention
    }

    /// Move a file or folder into the bin. Returns its new location.
    pub fn recycle(&self, item: &Path, guard: &PathGuard) -> Result<PathBuf> {
        self.recycle_at(item, guard, Utc::now())
    }

    /// [`recycle`](Self::recycle) with an explicit timestamp (tests, batch operations).
    /// Files land in `<bin>/<stamp>/<parent folder>/<file>`, folders in `<bin>/<stamp>/<folder>`.
    pub fn recycle_at(&self, item: &Path, guard: &PathGuard, now: DateTime<Utc>) -> Result<PathBuf> {
        let resolved = guard.check_deletable(item)?;
        let stamp_dir = self.path.join(now.format(STAMP_FORMAT).to_string());
        let name = resolved.file_name().ok_or_else(|| Error::InvalidPath {
            path: item.to_path_buf(),
            reason: "nothing to recycle",
        })?;
        let meta = std::fs::symlink_metadata(&resolved).map_err(|e| Error::io(&resolved, e))?;
        let mut target_dir = stamp_dir;
        if !meta.is_dir()
            && let Some(parent_name) = resolved.parent().and_then(Path::file_name)
        {
            target_dir.push(parent_name);
        }
        std::fs::create_dir_all(&target_dir).map_err(|e| Error::io(&target_dir, e))?;
        let target = unique(&target_dir.join(name));
        if meta.is_dir() {
            move_dir(&resolved, &target)?;
        } else {
            transfer_with(
                &resolved,
                &target,
                TransferMode::Move,
                &TransferOptions::default(),
            )?;
        }
        tracing::info!(from = %resolved.display(), to = %target.display(), "recycled");
        Ok(target)
    }

    /// Purge entries older than the retention period.
    pub fn cleanup(&self) -> Result<CleanupReport> {
        self.cleanup_at(Utc::now())
    }

    pub fn cleanup_at(&self, now: DateTime<Utc>) -> Result<CleanupReport> {
        let mut report = CleanupReport::default();
        let Some(retention) = self.retention else {
            return Ok(report);
        };
        let cutoff = now - chrono::Duration::from_std(retention).unwrap_or(chrono::Duration::MAX);
        for entry in std::fs::read_dir(&self.path).map_err(|e| Error::io(&self.path, e))? {
            let entry = entry.map_err(|e| Error::io(&self.path, e))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let when = NaiveDateTime::parse_from_str(&name, STAMP_FORMAT)
                .map(|n| n.and_utc())
                .ok()
                .or_else(|| entry.metadata().ok()?.modified().ok().map(DateTime::<Utc>::from));
            if when.is_some_and(|w| w < cutoff) {
                remove_any(&entry.path())?;
                report.removed += 1;
            } else {
                report.kept += 1;
            }
        }
        Ok(report)
    }

    /// Remove everything in the bin.
    pub fn empty(&self) -> Result<CleanupReport> {
        let mut report = CleanupReport::default();
        for entry in std::fs::read_dir(&self.path).map_err(|e| Error::io(&self.path, e))? {
            let entry = entry.map_err(|e| Error::io(&self.path, e))?;
            remove_any(&entry.path())?;
            report.removed += 1;
        }
        Ok(report)
    }
}

/// Permanently delete a file or folder (when no recycle bin is configured).
pub fn delete_permanently(item: &Path, guard: &PathGuard) -> Result<()> {
    let resolved = guard.check_deletable(item)?;
    remove_any(&resolved)
}

fn remove_any(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|e| Error::io(path, e))
}

/// `name`, or `name (2)`, `name (3)`… when taken.
fn unique(path: &Path) -> PathBuf {
    if std::fs::symlink_metadata(path).is_err() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .unwrap_or_else(|| path.to_path_buf())
}

fn move_dir(src: &Path, dst: &Path) -> Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_dir(src, dst)?;
            std::fs::remove_dir_all(src).map_err(|e| Error::io(src, e))
        }
        Err(e) => Err(Error::io(src, e)),
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(|e| Error::io(dst, e))?;
    for entry in std::fs::read_dir(src).map_err(|e| Error::io(src, e))? {
        let entry = entry.map_err(|e| Error::io(src, e))?;
        let ft = entry.file_type().map_err(|e| Error::io(entry.path(), e))?;
        let target = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &target).map_err(|e| Error::io(&target, e))?;
        }
    }
    Ok(())
}
