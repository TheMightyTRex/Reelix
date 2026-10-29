//! Path safety: every filesystem operation must stay inside configured roots.

use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

/// Restricts paths to a set of canonicalised root folders (library roots, download
/// folders, the recycle bin). Symlinks are resolved before the containment check,
/// so a link pointing outside the roots is rejected.
#[derive(Debug, Clone, Default)]
pub struct PathGuard {
    roots: Vec<PathBuf>,
}

impl PathGuard {
    /// Build from existing directories; each is canonicalised.
    pub fn new<I, P>(roots: I) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut guard = Self::default();
        for r in roots {
            guard.add_root(r.as_ref())?;
        }
        Ok(guard)
    }

    pub fn add_root(&mut self, root: &Path) -> Result<()> {
        let canon = std::fs::canonicalize(root).map_err(|e| Error::io(root, e))?;
        if !canon.is_dir() {
            return Err(Error::InvalidPath {
                path: root.to_path_buf(),
                reason: "root is not a directory",
            });
        }
        if !self.roots.contains(&canon) {
            self.roots.push(canon);
        }
        Ok(())
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The root containing the resolved `path`.
    pub fn root_of(&self, path: &Path) -> Option<&Path> {
        let resolved = resolve(path).ok()?;
        self.roots
            .iter()
            .filter(|r| resolved.starts_with(r))
            .max_by_key(|r| r.components().count())
            .map(PathBuf::as_path)
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.check(path).is_ok()
    }

    /// Resolve `path` (which need not exist yet) and ensure it lies inside a root.
    /// Returns the resolved absolute path.
    pub fn check(&self, path: &Path) -> Result<PathBuf> {
        let resolved = resolve(path)?;
        if self.roots.iter().any(|r| resolved.starts_with(r)) {
            Ok(resolved)
        } else {
            Err(Error::OutsideRoots(path.to_path_buf()))
        }
    }

    /// Like [`check`](Self::check) but the path must exist.
    pub fn check_existing(&self, path: &Path) -> Result<PathBuf> {
        std::fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
        self.check(path)
    }

    /// For deletions: the path must exist, be inside a root and not be a root itself.
    pub fn check_deletable(&self, path: &Path) -> Result<PathBuf> {
        let resolved = self.check_existing(path)?;
        if self.roots.iter().any(|r| r == &resolved) {
            return Err(Error::InvalidPath {
                path: path.to_path_buf(),
                reason: "refusing to delete a root folder",
            });
        }
        Ok(resolved)
    }
}

/// Canonicalise the deepest existing ancestor and append the remaining (plain)
/// components. `..` in the non-existing tail and dangling symlinks are rejected.
fn resolve(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::InvalidPath {
            path: path.to_path_buf(),
            reason: "path must be absolute",
        });
    }
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor = Some(path);
    while let Some(p) = cursor {
        if std::fs::symlink_metadata(p).is_ok() {
            let mut base = std::fs::canonicalize(p).map_err(|_| Error::InvalidPath {
                path: path.to_path_buf(),
                reason: "dangling symlink or unreadable path component",
            })?;
            for part in tail.iter().rev() {
                base.push(part);
            }
            return Ok(base);
        }
        match p.components().next_back() {
            Some(Component::Normal(name)) => tail.push(name),
            Some(Component::CurDir) => {}
            _ => {
                return Err(Error::InvalidPath {
                    path: path.to_path_buf(),
                    reason: "relative components are not allowed",
                });
            }
        }
        cursor = p.parent();
    }
    Err(Error::InvalidPath {
        path: path.to_path_buf(),
        reason: "no existing ancestor",
    })
}
