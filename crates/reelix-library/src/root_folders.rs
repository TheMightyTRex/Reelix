//! Root folders: CRUD, free space and unmapped folders.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use reelix_api_types::library::{RootFolderDto, UnmappedFolderDto};
use reelix_core::MediaKind;
use reelix_import::files::disk_space;

use crate::LibraryService;
use crate::error::{LibraryError, Result};
use crate::repo::misc::{self, RootFolder};

impl LibraryService {
    /// Root folders with free space and unmapped sub-folders.
    pub async fn root_folders(&self) -> Result<Vec<RootFolderDto>> {
        let roots = misc::root_folders(&self.db).await?;
        let mapped = self.mapped_paths().await?;
        let mut out = Vec::with_capacity(roots.len());
        for r in roots {
            out.push(root_dto(&r, &mapped).await);
        }
        Ok(out)
    }

    pub async fn root_folder(&self, id: i64) -> Result<RootFolderDto> {
        let r = misc::root_folder(&self.db, id)
            .await?
            .ok_or_else(|| LibraryError::not_found("root folder", id))?;
        Ok(root_dto(&r, &self.mapped_paths().await?).await)
    }

    pub async fn add_root_folder(&self, path: &str, kind: MediaKind) -> Result<RootFolderDto> {
        let trimmed = path.trim();
        let p = Path::new(trimmed);
        if !p.is_absolute() {
            return Err(LibraryError::validation("root folder path must be absolute"));
        }
        if !p.is_dir() {
            return Err(LibraryError::validation(format!(
                "{trimmed} does not exist or is not a directory"
            )));
        }
        let normalized = normalize(trimmed);
        // Overlaps are checked against root folders of every kind (books and music too).
        crate::content_roots::check_overlap(&self.db, &normalized).await?;
        let id = misc::insert_root_folder(&self.db, &normalized, kind).await?;
        self.emit("root_folder", "created", id);
        self.root_folder(id).await
    }

    /// Refuses while movies/series still use the folder.
    pub async fn delete_root_folder(&self, id: i64) -> Result<()> {
        let used = misc::root_folder_usage(&self.db, id).await?;
        if used > 0 {
            return Err(LibraryError::Conflict(format!(
                "root folder is used by {used} movies/series; move or delete them first"
            )));
        }
        if !misc::delete_root_folder(&self.db, id).await? {
            return Err(LibraryError::not_found("root folder", id));
        }
        self.emit("root_folder", "deleted", id);
        Ok(())
    }

    pub(crate) async fn require_root(&self, id: i64, kind: MediaKind) -> Result<RootFolder> {
        let r = misc::root_folder(&self.db, id)
            .await?
            .ok_or_else(|| LibraryError::validation(format!("root folder {id} does not exist")))?;
        if r.media_kind != kind {
            return Err(LibraryError::validation(format!(
                "root folder {} is for {}",
                r.path,
                crate::repo::kind_str(r.media_kind)
            )));
        }
        Ok(r)
    }

    /// Every movie/series folder (normalised).
    pub(crate) async fn mapped_paths(&self) -> Result<HashSet<String>> {
        let rows: Vec<String> = sqlx::query_scalar("SELECT path FROM movies UNION SELECT path FROM series")
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows.into_iter().map(|p| normalize(&p)).collect())
    }
}

/// Trailing separators removed.
pub fn normalize(p: &str) -> String {
    let t = p.trim();
    let s = t.trim_end_matches(['/', '\\']);
    if s.is_empty() { t.to_owned() } else { s.to_owned() }
}

pub(crate) fn unmapped_folders(root: &Path, mapped: &HashSet<String>) -> Vec<UnmappedFolderDto> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<UnmappedFolderDto> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.starts_with('@') || name.starts_with('#') || is_system_dir(&name)
            {
                return None;
            }
            let path: PathBuf = e.path();
            let p = normalize(&path.to_string_lossy());
            (!mapped.contains(&p)).then_some(UnmappedFolderDto { name, path: p })
        })
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

fn is_system_dir(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "lost+found" | "$recycle.bin" | "system volume information" | "recycle bin" | ".trash" | "extras"
    )
}

async fn root_dto(r: &RootFolder, mapped: &HashSet<String>) -> RootFolderDto {
    let path = PathBuf::from(&r.path);
    let mapped = mapped.clone();
    let (accessible, space, unmapped) = tokio::task::spawn_blocking(move || {
        let accessible = path.is_dir();
        let space = accessible.then(|| disk_space(&path).ok()).flatten();
        let unmapped = if accessible {
            unmapped_folders(&path, &mapped)
        } else {
            Vec::new()
        };
        (accessible, space, unmapped)
    })
    .await
    .unwrap_or((false, None, Vec::new()));
    RootFolderDto {
        id: r.id,
        path: r.path.clone(),
        media_kind: r.media_kind,
        accessible,
        free_space: space.map(|s| s.available),
        total_space: space.map(|s| s.total),
        unmapped_folders: unmapped,
    }
}
