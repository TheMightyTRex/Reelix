//! Free-space queries and filesystem identity.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskSpace {
    /// Bytes available to unprivileged users.
    pub available: u64,
    pub total: u64,
}

/// Space on the filesystem holding `path` (or its nearest existing ancestor, so a
/// not-yet-created destination folder can be checked).
pub fn disk_space(path: &Path) -> Result<DiskSpace> {
    let existing = path
        .ancestors()
        .find(|p| p.exists())
        .ok_or_else(|| Error::InvalidPath {
            path: path.to_path_buf(),
            reason: "no existing ancestor",
        })?;
    let stats = fs4::statvfs(existing).map_err(|e| Error::io(existing, e))?;
    Ok(DiskSpace {
        available: stats.available_space(),
        total: stats.total_space(),
    })
}

/// Whether two existing paths live on the same filesystem (hardlinks possible).
/// `None` where the platform gives no cheap answer.
pub fn same_filesystem(a: &Path, b: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dev = |p: &Path| {
            p.ancestors()
                .find_map(|p| std::fs::metadata(p).ok())
                .map(|m| m.dev())
        };
        Some(dev(a)? == dev(b)?)
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        None
    }
}
