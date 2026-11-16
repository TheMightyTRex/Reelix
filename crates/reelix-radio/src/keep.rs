//! Keeping only the newest N episodes of a show (`radio_shows.keep_episodes`, RFX-106).
//!
//! For a show with a limit, the episodes with a file (filed by Reelix or matched in the
//! podcast library) are put newest first: by first broadcast, else the library file's publish
//! date, else id. Every episode beyond the limit loses its file:
//!
//! 1. Each known path of the file (the filed path, the library file's path) is deleted from
//!    disk only when it is a plain file inside the show's podcast library folder
//!    ([`RadioService::library_folder_for`]) once every link is resolved. A path that is a link
//!    itself, or that leads outside the folder, is never touched, and the episode is left as
//!    it is.
//! 2. The filed path is cleared and the library file record is unlinked and removed.
//! 3. The episode is unmonitored, so it is not searched for or fetched again.
//!
//! It runs after an episode is filed, when a limit is saved on a show and with the hourly
//! catch-up job ([`RadioService::prune_all_shows`]).

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use reelix_api_types::radio::{RadioEpisodeDto, RadioEpisodeFileDto};

use crate::error::Result;
use crate::{RadioService, entities, repo};

/// What applying the limits did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeepSummary {
    /// Episodes that lost their file and were unmonitored.
    pub episodes: u32,
    /// Files deleted from disk.
    pub files_deleted: u32,
    /// Episodes beyond the limit that were left alone (a file outside the folder, a link, or a
    /// file that could not be reached or deleted).
    pub skipped: u32,
}

impl KeepSummary {
    fn add(&mut self, o: KeepSummary) {
        self.episodes += o.episodes;
        self.files_deleted += o.files_deleted;
        self.skipped += o.skipped;
    }
}

/// What a path on disk is, seen from the podcast library folder.
#[derive(Debug, PartialEq, Eq)]
enum OnDisk {
    /// Nothing is there.
    Gone,
    /// A plain file inside the folder (its resolved path).
    Inside(PathBuf),
    /// A link, a folder, or something outside the folder: never deleted.
    Refused,
}

/// Looks at `path` without following a link at its end; `root` is the resolved folder.
fn on_disk(root: &Path, path: &Path) -> std::io::Result<OnDisk> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(OnDisk::Gone),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Ok(OnDisk::Refused);
    }
    // Resolves links in the folders above the file, so a folder link out of the library is
    // seen for what it is.
    let resolved = std::fs::canonicalize(path)?;
    Ok(if resolved.starts_with(root) {
        OnDisk::Inside(resolved)
    } else {
        OnDisk::Refused
    })
}

/// The date an episode is ordered by: first broadcast, else the library file's publish date.
fn episode_date(e: &RadioEpisodeDto, f: Option<&RadioEpisodeFileDto>) -> Option<DateTime<Utc>> {
    e.first_broadcast.or_else(|| f.and_then(|f| f.published_at))
}

/// Newest first: by date (episodes without one last), then the higher id.
fn newest_first(
    a: &(RadioEpisodeDto, Option<RadioEpisodeFileDto>),
    b: &(RadioEpisodeDto, Option<RadioEpisodeFileDto>),
) -> Ordering {
    episode_date(&b.0, b.1.as_ref())
        .cmp(&episode_date(&a.0, a.1.as_ref()))
        .then(b.0.id.cmp(&a.0.id))
}

impl RadioService {
    /// Applies `keep_episodes` of one show (see the module docs). Does nothing for a show
    /// without a limit, or without a podcast library folder to check the paths against.
    pub async fn prune_show(&self, show_id: i64) -> Result<KeepSummary> {
        let mut sum = KeepSummary::default();
        let show = repo::shows::require(&self.db, show_id).await?;
        let Some(keep) = show
            .keep_episodes
            .and_then(|k| usize::try_from(k).ok())
            .filter(|k| *k >= 1)
        else {
            return Ok(sum);
        };
        let mut with_file = Vec::new();
        for e in repo::episodes::for_show(&self.db, show_id).await? {
            if e.file_id.is_none() && e.filed_path.is_none() {
                continue;
            }
            let f = match e.file_id {
                Some(id) => repo::files::get(&self.db, id).await?,
                None => None,
            };
            with_file.push((e, f));
        }
        if with_file.len() <= keep {
            return Ok(sum);
        }
        let Some(folder) = self.library_folder_for(&show).await? else {
            tracing::warn!(
                show_id,
                "radio keep episodes: the show has no podcast library folder"
            );
            return Ok(sum);
        };
        let root = match std::fs::canonicalize(&folder) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(show_id, folder = %folder.display(), error = %e, "radio keep episodes: podcast library folder not readable");
                return Ok(sum);
            }
        };
        with_file.sort_by(newest_first);

        let mut pruned = Vec::new();
        for (e, f) in with_file.into_iter().skip(keep) {
            let filed = e.filed_path.as_deref().map(PathBuf::from);
            let library = f.as_ref().and_then(|f| f.path.as_deref()).map(PathBuf::from);
            let mut delete = Vec::new();
            let mut refused = false;
            let mut library_gone = library.is_none();
            for (p, is_library) in [(filed, false), (library, true)]
                .into_iter()
                .filter_map(|(p, l)| p.map(|p| (p, l)))
            {
                match on_disk(&root, &p) {
                    Ok(OnDisk::Gone) => library_gone |= is_library,
                    Ok(OnDisk::Inside(r)) => delete.push(r),
                    Ok(OnDisk::Refused) => {
                        tracing::warn!(show_id, episode_id = e.id, path = %p.display(), "radio keep episodes: file outside the podcast library folder left alone");
                        refused = true;
                    }
                    Err(err) => {
                        tracing::warn!(show_id, episode_id = e.id, path = %p.display(), error = %err, "radio keep episodes: file not checked");
                        refused = true;
                    }
                }
            }
            // A library file Reelix cannot see (the library may name it from another mount)
            // and nothing else to delete: the file may still be there, so the episode is kept.
            if !refused && delete.is_empty() && e.file_id.is_some() && library_gone {
                tracing::warn!(
                    show_id,
                    episode_id = e.id,
                    "radio keep episodes: library file not found from Reelix, left alone"
                );
                refused = true;
            }
            if refused {
                sum.skipped += 1;
                continue;
            }
            delete.sort();
            delete.dedup();
            let mut failed = false;
            for p in &delete {
                match std::fs::remove_file(p) {
                    Ok(()) => {
                        sum.files_deleted += 1;
                        tracing::info!(show_id, episode_id = e.id, keep, path = %p.display(), "radio keep episodes: older episode file deleted");
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        tracing::warn!(show_id, episode_id = e.id, path = %p.display(), error = %err, "radio keep episodes: file not deleted");
                        failed = true;
                    }
                }
            }
            if failed {
                sum.skipped += 1;
                continue;
            }
            repo::episodes::set_filed(&self.db, e.id, None).await?;
            if let Some(file_id) = e.file_id {
                repo::episodes::set_file(&self.db, e.id, None, false).await?;
                repo::files::delete(&self.db, &[file_id]).await?;
                self.emit(entities::EPISODE_FILE, "deleted", file_id);
            }
            pruned.push(e.id);
        }
        if !pruned.is_empty() {
            sum.episodes = pruned.len() as u32;
            self.apply_monitored(show_id, &pruned, false).await?;
            if let Err(e) = self.request_library_scan().await {
                tracing::warn!(error = %e, "radio keep episodes: library scan request failed");
            }
        }
        Ok(sum)
    }

    /// Applies `keep_episodes` of every show that has one (the hourly catch-up job). A failure
    /// on one show is logged and the others carry on.
    pub async fn prune_all_shows(&self) -> Result<KeepSummary> {
        let mut sum = KeepSummary::default();
        for id in repo::shows::with_keep_limit(&self.db).await? {
            match self.prune_show(id).await {
                Ok(s) => sum.add(s),
                Err(e) => tracing::warn!(show_id = id, error = %e, "radio keep episodes: show not checked"),
            }
        }
        Ok(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_disk_refuses_links_and_outside_paths() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("library");
        let outside = base.path().join("elsewhere");
        std::fs::create_dir_all(root.join("Show")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("Show/a.mp3"), b"a").unwrap();
        std::fs::write(outside.join("b.mp3"), b"b").unwrap();
        let root = std::fs::canonicalize(&root).unwrap();

        assert!(matches!(
            on_disk(&root, &root.join("Show/a.mp3")).unwrap(),
            OnDisk::Inside(_)
        ));
        assert_eq!(on_disk(&root, &root.join("Show/none.mp3")).unwrap(), OnDisk::Gone);
        assert_eq!(on_disk(&root, &outside.join("b.mp3")).unwrap(), OnDisk::Refused);
        // `..` out of the folder.
        assert_eq!(
            on_disk(&root, &root.join("../elsewhere/b.mp3")).unwrap(),
            OnDisk::Refused
        );
        #[cfg(unix)]
        {
            // A link to a file outside, and a folder link out of the library.
            std::os::unix::fs::symlink(outside.join("b.mp3"), root.join("Show/link.mp3")).unwrap();
            assert_eq!(
                on_disk(&root, &root.join("Show/link.mp3")).unwrap(),
                OnDisk::Refused
            );
            std::os::unix::fs::symlink(&outside, root.join("Out")).unwrap();
            assert_eq!(on_disk(&root, &root.join("Out/b.mp3")).unwrap(), OnDisk::Refused);
        }
    }
}
