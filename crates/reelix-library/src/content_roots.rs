//! Root folders of every content kind, for the books and music services.
//!
//! All kinds share the `root_folders` table, so overlap checks span movies, series, books and
//! music. The movie/series API ([`LibraryService::root_folders`](crate::LibraryService)) only
//! ever sees movie/series rows.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use reelix_api_types::content::ContentRootFolderDto;
use reelix_core::ContentKind;
use reelix_db::{Db, now_ms};
use reelix_import::files::{PathGuard, disk_space};
use sqlx::Row;

use crate::error::{LibraryError, Result};
use crate::root_folders::unmapped_folders;

pub use crate::root_folders::normalize;

/// A root folder of any kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRoot {
    pub id: i64,
    pub path: String,
    pub kind: ContentKind,
}

/// Root folders of the given kinds, ordered by path.
pub async fn list(db: &Db, kinds: &[ContentKind]) -> Result<Vec<ContentRoot>> {
    let rows = sqlx::query("SELECT id, path, media_kind FROM root_folders ORDER BY path")
        .fetch_all(db.pool())
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let kind: String = r.try_get("media_kind")?;
        let Some(kind) = ContentKind::parse(&kind) else {
            continue;
        };
        if kinds.contains(&kind) {
            out.push(ContentRoot {
                id: r.try_get("id")?,
                path: r.try_get("path")?,
                kind,
            });
        }
    }
    Ok(out)
}

pub async fn get(db: &Db, id: i64) -> Result<Option<ContentRoot>> {
    Ok(list(db, &ContentKind::ALL)
        .await?
        .into_iter()
        .find(|r| r.id == id))
}

/// Fails with `Validation` unless `path` lies strictly inside one of `roots`: absolute, free of
/// `..` components and, for a root that exists on disk, still inside it once symlinks are
/// resolved.
pub fn ensure_inside<'a>(path: &str, roots: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(LibraryError::validation("path must be absolute"));
    }
    if p.components().any(|c| c == Component::ParentDir) {
        return Err(LibraryError::validation("path must not contain `..`"));
    }
    let inside = roots
        .into_iter()
        .map(Path::new)
        .any(|root| p != root && p.starts_with(root) && (!root.is_dir() || resolves_inside(p, root)));
    if inside {
        Ok(())
    } else {
        Err(LibraryError::validation(format!(
            "{path} is not inside a root folder"
        )))
    }
}

/// `path` with symlinks resolved is still strictly inside the existing directory `root`.
fn resolves_inside(path: &Path, root: &Path) -> bool {
    let Ok(guard) = PathGuard::new([root]) else {
        return false;
    };
    guard
        .check(path)
        .is_ok_and(|resolved| guard.roots().iter().all(|r| r != &resolved))
}

/// [`ensure_inside`] over every root folder of `kinds`.
pub async fn ensure_inside_kinds(db: &Db, path: &str, kinds: &[ContentKind]) -> Result<()> {
    let roots = list(db, kinds).await?;
    ensure_inside(path, roots.iter().map(|r| r.path.as_str()))
}

/// Fails with `Conflict` when `path` (normalised) overlaps any existing root folder.
pub(crate) async fn check_overlap(db: &Db, normalized: &str) -> Result<()> {
    for r in list(db, &ContentKind::ALL).await? {
        let existing = Path::new(&r.path);
        if Path::new(normalized).starts_with(existing) || existing.starts_with(normalized) {
            return Err(LibraryError::Conflict(format!(
                "{normalized} overlaps the existing root folder {}",
                r.path
            )));
        }
    }
    Ok(())
}

/// Adds a root folder: absolute, existing directory, no overlap with any root folder of any
/// kind. Returns the id.
pub async fn add(db: &Db, path: &str, kind: ContentKind) -> Result<i64> {
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
    check_overlap(db, &normalized).await?;
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&normalized)
    .bind(kind.as_str())
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            LibraryError::Conflict(format!("root folder {normalized:?} already exists"))
        }
        _ => e.into(),
    })?;
    Ok(res.last_insert_rowid())
}

/// Deletes a root folder row (callers check their own usage first). False when missing.
pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM root_folders WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}

/// DTO with free space and unmapped sub-folders (`mapped` = normalised media folders of the
/// caller, e.g. every author or artist path).
pub async fn dto(root: &ContentRoot, mapped: &HashSet<String>) -> ContentRootFolderDto {
    let path = PathBuf::from(&root.path);
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
    ContentRootFolderDto {
        id: root.id,
        path: root.path.clone(),
        kind: root.kind,
        accessible,
        free_space: space.map(|s| s.available),
        total_space: space.map(|s| s.total),
        unmapped_folders: unmapped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kinds_share_one_table() {
        let db = Db::in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let books = dir.path().join("books");
        let music = dir.path().join("music");
        std::fs::create_dir_all(books.join("Some Author")).unwrap();
        std::fs::create_dir_all(&music).unwrap();
        let b = add(&db, &books.to_string_lossy(), ContentKind::Ebook)
            .await
            .unwrap();
        let m = add(&db, &format!("{}/", music.display()), ContentKind::Music)
            .await
            .unwrap();
        assert!(matches!(
            add(
                &db,
                &books.join("Some Author").to_string_lossy(),
                ContentKind::Audiobook
            )
            .await,
            Err(LibraryError::Conflict(_))
        ));
        assert!(matches!(
            add(&db, "relative", ContentKind::Music).await,
            Err(LibraryError::Validation(_))
        ));
        let music_roots = list(&db, &[ContentKind::Music]).await.unwrap();
        assert_eq!(music_roots.len(), 1);
        assert_eq!(music_roots[0].path, music.to_string_lossy());
        let root = get(&db, b).await.unwrap().unwrap();
        assert_eq!(root.kind, ContentKind::Ebook);
        let d = dto(&root, &HashSet::new()).await;
        assert!(d.accessible);
        assert_eq!(d.unmapped_folders.len(), 1);
        assert!(delete(&db, m).await.unwrap());
        assert!(!delete(&db, m).await.unwrap());
    }

    #[test]
    fn ensure_inside_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let root_s = root.to_string_lossy().into_owned();
        let roots = || [root_s.as_str()];
        let inside = |p: &Path| ensure_inside(&p.to_string_lossy(), roots());
        assert!(inside(&root.join("Film (2020)")).is_ok());
        assert!(inside(&root.join("a/b")).is_ok());
        assert!(inside(&root).is_err());
        assert!(inside(&root.join("../../etc")).is_err());
        assert!(inside(dir.path()).is_err());
        assert!(ensure_inside("relative/path", roots()).is_err());
        // Lexically inside, but a symlink leads out of the root.
        #[cfg(unix)]
        {
            let outside = dir.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            assert!(inside(&root.join("link")).is_err());
            assert!(inside(&root.join("link/sub")).is_err());
        }
        // Roots not on disk are checked lexically.
        let nowhere = if cfg!(windows) {
            r"C:\nowhere\movies"
        } else {
            "/nowhere/movies"
        };
        assert!(ensure_inside(&format!("{nowhere}/Film"), [nowhere]).is_ok());
        assert!(ensure_inside(&format!("{nowhere}/../etc"), [nowhere]).is_err());
    }
}
