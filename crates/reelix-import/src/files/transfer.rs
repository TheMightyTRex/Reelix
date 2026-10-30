//! Hardlink / copy / move with cross-device fallback and atomic temp-then-rename.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Requested import mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferMode {
    /// Hardlink only; fails when not possible (e.g. across filesystems).
    Hardlink,
    /// Hardlink when possible (keeps seeding), else copy. The default.
    #[default]
    HardlinkOrCopy,
    Copy,
    /// Rename; copy + delete when crossing filesystems.
    Move,
}

/// What actually happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferMethod {
    Hardlinked,
    Copied,
    Moved,
    /// Cross-device move: copied, then the source was removed.
    MovedViaCopy,
    /// Source and destination are already the same file.
    AlreadyInPlace,
}

/// Optional unix ownership / mode applied to imported files and created folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PermissionSettings {
    /// e.g. `0o644`
    pub file_mode: Option<u32>,
    /// e.g. `0o755`, applied to folders this transfer creates.
    pub dir_mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferOptions {
    /// Atomically replace an existing destination (upgrades after recycling).
    pub overwrite: bool,
    pub permissions: PermissionSettings,
    /// Keep the source modification time on copies.
    pub preserve_mtime: bool,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            overwrite: false,
            permissions: PermissionSettings::default(),
            preserve_mtime: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferOutcome {
    pub method: TransferMethod,
    pub destination: PathBuf,
    pub bytes: u64,
}

/// Low-level operations, injectable so fallbacks can be tested without two filesystems.
pub(crate) trait FsOps {
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()>;
    fn rename(&self, src: &Path, dst: &Path) -> io::Result<()>;
}

pub(crate) struct RealFs;

impl FsOps for RealFs {
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        std::fs::hard_link(src, dst)
    }
    fn rename(&self, src: &Path, dst: &Path) -> io::Result<()> {
        std::fs::rename(src, dst)
    }
}

/// Transfer `src` to `dst` with default options (no overwrite).
pub fn transfer(src: &Path, dst: &Path, mode: TransferMode) -> Result<TransferOutcome> {
    transfer_with(src, dst, mode, &TransferOptions::default())
}

/// Transfer `src` (a file) to `dst`. Parent folders are created; copies go to a hidden
/// temp file next to `dst` and are renamed into place, so `dst` is never partial.
/// Callers check both paths with a [`PathGuard`](super::PathGuard) first.
pub fn transfer_with(
    src: &Path,
    dst: &Path,
    mode: TransferMode,
    opts: &TransferOptions,
) -> Result<TransferOutcome> {
    transfer_impl(&RealFs, src, dst, mode, opts)
}

pub(crate) fn transfer_impl(
    ops: &dyn FsOps,
    src: &Path,
    dst: &Path,
    mode: TransferMode,
    opts: &TransferOptions,
) -> Result<TransferOutcome> {
    let meta = std::fs::metadata(src).map_err(|e| Error::io(src, e))?;
    if !meta.is_file() {
        return Err(Error::InvalidPath {
            path: src.to_path_buf(),
            reason: "source is not a regular file",
        });
    }
    let bytes = meta.len();
    let outcome = |method| TransferOutcome {
        method,
        destination: dst.to_path_buf(),
        bytes,
    };
    if dst.exists() {
        if same_file(src, dst) {
            return Ok(outcome(TransferMethod::AlreadyInPlace));
        }
        if !opts.overwrite {
            return Err(Error::DestinationExists(dst.to_path_buf()));
        }
    }
    let parent = dst.parent().ok_or_else(|| Error::InvalidPath {
        path: dst.to_path_buf(),
        reason: "destination has no parent folder",
    })?;
    create_dirs(parent, &opts.permissions)?;

    let method = match mode {
        TransferMode::Hardlink => {
            link_atomic(ops, src, dst).map_err(|source| Error::Hardlink {
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                source,
            })?;
            TransferMethod::Hardlinked
        }
        TransferMode::HardlinkOrCopy => match link_atomic(ops, src, dst) {
            Ok(()) => TransferMethod::Hardlinked,
            Err(e) => {
                tracing::debug!(src = %src.display(), error = %e, "hardlink failed, copying");
                copy_atomic(src, dst, opts)?;
                TransferMethod::Copied
            }
        },
        TransferMode::Copy => {
            copy_atomic(src, dst, opts)?;
            TransferMethod::Copied
        }
        TransferMode::Move => match ops.rename(src, dst) {
            Ok(()) => TransferMethod::Moved,
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                copy_atomic(src, dst, opts)?;
                std::fs::remove_file(src).map_err(|e| Error::io(src, e))?;
                TransferMethod::MovedViaCopy
            }
            Err(e) => return Err(Error::io(src, e)),
        },
    };
    // A hardlink shares the inode with the seeding source: leave its mode alone.
    if method != TransferMethod::Hardlinked {
        apply_permissions(dst, &opts.permissions)?;
    }
    Ok(outcome(method))
}

/// Same path after canonicalisation, or (unix) the same inode, e.g. an existing hardlink.
fn same_file(a: &Path, b: &Path) -> bool {
    if matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y) {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    false
}

fn temp_path(dst: &Path) -> PathBuf {
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    dst.with_file_name(format!(".{name}.reelix-{}-{nanos}.tmp", std::process::id()))
}

fn link_atomic(ops: &dyn FsOps, src: &Path, dst: &Path) -> io::Result<()> {
    let tmp = temp_path(dst);
    ops.hard_link(src, &tmp)?;
    ops.rename(&tmp, dst).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn copy_atomic(src: &Path, dst: &Path, opts: &TransferOptions) -> Result<()> {
    let tmp = temp_path(dst);
    let result = (|| -> Result<()> {
        let copied = std::fs::copy(src, &tmp).map_err(|e| Error::io(&tmp, e))?;
        let expected = std::fs::metadata(src).map_err(|e| Error::io(src, e))?.len();
        if copied != expected {
            return Err(Error::io(
                &tmp,
                io::Error::other(format!("copied {copied} of {expected} bytes")),
            ));
        }
        // Windows only flushes a handle opened for writing (else "Access is denied").
        std::fs::OpenOptions::new()
            .write(true)
            .open(&tmp)
            .and_then(|f| f.sync_all())
            .map_err(|e| Error::io(&tmp, e))?;
        if opts.preserve_mtime
            && let Ok(m) = std::fs::metadata(src)
        {
            let mtime = filetime::FileTime::from_last_modification_time(&m);
            let _ = filetime::set_file_mtime(&tmp, mtime);
        }
        // Renaming within dst's folder never crosses devices.
        std::fs::rename(&tmp, dst).map_err(|e| Error::io(dst, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Create `dir` and missing ancestors, applying `dir_mode` / ownership to new ones.
fn create_dirs(dir: &Path, perms: &PermissionSettings) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut missing = Vec::new();
    let mut cur = Some(dir);
    while let Some(p) = cur {
        if p.exists() {
            break;
        }
        missing.push(p.to_path_buf());
        cur = p.parent();
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    for p in missing.iter().rev() {
        apply_permissions(p, perms)?;
    }
    Ok(())
}

/// Apply mode/ownership (unix only; a no-op elsewhere or when nothing is configured).
#[cfg(unix)]
pub fn apply_permissions(path: &Path, perms: &PermissionSettings) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    let mode = if meta.is_dir() {
        perms.dir_mode
    } else {
        perms.file_mode
    };
    if let Some(mode) = mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| Error::io(path, e))?;
    }
    if perms.uid.is_some() || perms.gid.is_some() {
        std::os::unix::fs::chown(path, perms.uid, perms.gid).map_err(|e| Error::io(path, e))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn apply_permissions(_path: &Path, _perms: &PermissionSettings) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates a destination on another filesystem.
    struct CrossDevice;
    impl FsOps for CrossDevice {
        fn hard_link(&self, _: &Path, _: &Path) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::CrossesDevices))
        }
        fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::CrossesDevices))
        }
    }

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("dl/Movie.mkv");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"video-bytes").unwrap();
        let dst = dir.path().join("lib/Movie (2010)/Movie (2010).mkv");
        (dir, src, dst)
    }

    #[test]
    fn hardlink_or_copy_falls_back_across_devices() {
        let (_d, src, dst) = setup();
        let out = transfer_impl(
            &CrossDevice,
            &src,
            &dst,
            TransferMode::HardlinkOrCopy,
            &TransferOptions::default(),
        )
        .unwrap();
        assert_eq!(out.method, TransferMethod::Copied);
        assert_eq!(std::fs::read(&dst).unwrap(), b"video-bytes");
        assert!(src.exists());
    }

    #[test]
    fn strict_hardlink_fails_across_devices() {
        let (_d, src, dst) = setup();
        let err = transfer_impl(
            &CrossDevice,
            &src,
            &dst,
            TransferMode::Hardlink,
            &TransferOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Hardlink { .. }));
        assert!(!dst.exists());
    }

    #[test]
    fn move_falls_back_to_copy_and_delete() {
        let (_d, src, dst) = setup();
        let out = transfer_impl(
            &CrossDevice,
            &src,
            &dst,
            TransferMode::Move,
            &TransferOptions::default(),
        )
        .unwrap();
        assert_eq!(out.method, TransferMethod::MovedViaCopy);
        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), b"video-bytes");
        // No temp files left behind.
        let leftovers = std::fs::read_dir(dst.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1);
    }
}
