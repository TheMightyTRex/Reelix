//! The podcast library seam: reading the Audiobookshelf podcast library into
//! `radio_episode_files`, matching files to episodes, and the review of what is left over.
//!
//! [`RadioLibrarySync`] is the extension point the Audiobookshelf implementation plugs into
//! (attached with [`RadioService::set_library_sync`]; the server's lives in
//! `reelix-server/src/integrations/radio_library.rs`); until then the `radio_library_sync`
//! task runs [`NoopLibrarySync`]. A reader hands what it read to
//! [`RadioService::sync_library_files`], which upserts, removes unseen files, matches
//! ([`RadioService::match_library`]) and records the sync.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use reelix_api_types::radio::{RadioEpisodeFileDto, RadioLibraryReviewDto};
use reelix_db::from_ms;

use crate::error::Result;
use crate::{RadioService, entities, keys, repo};

/// What one sync did (logged by the job).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibrarySyncSummary {
    pub files_seen: u32,
    pub files_removed: u32,
    pub episodes_matched: u32,
}

/// Reads the podcast library and re-matches files to episodes.
///
/// An implementation typically: lists the library's podcast items and their episodes,
/// calls [`RadioService::upsert_episode_files`] with every episode that has an audio file
/// (same `seen_at` for the whole run), [`RadioService::remove_unseen_files`] with that
/// `seen_at`, links files with [`RadioService::link_file_auto`], and finally
/// [`RadioService::mark_library_synced`].
#[async_trait::async_trait]
pub trait RadioLibrarySync: Send + Sync + 'static {
    async fn sync(&self, svc: &RadioService) -> anyhow::Result<LibrarySyncSummary>;

    /// Asks the podcast library to scan for new files (after filing). The default does
    /// nothing.
    async fn request_scan(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The default: no podcast library is connected.
pub struct NoopLibrarySync;

#[async_trait::async_trait]
impl RadioLibrarySync for NoopLibrarySync {
    async fn sync(&self, _svc: &RadioService) -> anyhow::Result<LibrarySyncSummary> {
        tracing::debug!("radio library sync: no podcast library reader attached");
        Ok(LibrarySyncSummary::default())
    }
}

/// One episode file as read from the podcast library.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewEpisodeFile {
    pub abs_item_id: String,
    pub abs_episode_id: String,
    pub item_title: Option<String>,
    pub title: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub guid: Option<String>,
    pub enclosure_url: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<u32>,
    pub path: Option<String>,
    pub size: Option<u64>,
}

/// The comparison key used for show titles (`radio_shows.clean_title`); apply it to library
/// item titles before comparing.
pub fn clean_title(title: &str) -> String {
    crate::convert::clean_title(title)
}

impl RadioService {
    /// Runs the attached [`RadioLibrarySync`].
    pub async fn run_library_sync(&self) -> anyhow::Result<LibrarySyncSummary> {
        let sync = self.library_sync();
        sync.sync(self).await
    }

    /// Asks the attached [`RadioLibrarySync`] to scan the podcast library (after filing).
    pub async fn request_library_scan(&self) -> anyhow::Result<()> {
        let sync = self.library_sync();
        sync.request_scan().await
    }

    /// One whole sync from what the library holds now: saves `files` (episodes with an audio
    /// file), removes the files no longer there (their episodes return to Wanted or Not
    /// requested), matches files to episodes ([`RadioService::match_library`]) and records
    /// the sync.
    pub async fn sync_library_files(&self, files: &[NewEpisodeFile]) -> Result<LibrarySyncSummary> {
        let now = self.now();
        let ids = self.upsert_episode_files(files, now).await?;
        let removed = self.remove_unseen_files(now).await?;
        let matched = self.match_library().await?;
        self.mark_library_synced(now).await?;
        Ok(LibrarySyncSummary {
            files_seen: ids.len() as u32,
            files_removed: removed,
            episodes_matched: matched,
        })
    }

    /// Shows whose follow choice is not `none` (they need the podcast library).
    pub async fn followed_show_count(&self) -> Result<u32> {
        repo::files::followed_shows(&self.db).await
    }

    /// Saves files seen by a sync (by item + episode id) with `seen_at`. Returns their ids in
    /// order.
    pub async fn upsert_episode_files(
        &self,
        files: &[NewEpisodeFile],
        seen_at: DateTime<Utc>,
    ) -> Result<Vec<i64>> {
        let mut ids = Vec::with_capacity(files.len());
        for f in files {
            let id = repo::files::upsert(&self.db, f, seen_at.timestamp_millis()).await?;
            self.emit(entities::EPISODE_FILE, "updated", id);
            ids.push(id);
        }
        Ok(ids)
    }

    /// Deletes files a sync no longer saw (`seen_at` before `seen_before`). Their episodes
    /// return to Wanted or Not requested (hand-set links are dropped too: the file is gone).
    /// Returns the number removed.
    pub async fn remove_unseen_files(&self, seen_before: DateTime<Utc>) -> Result<u32> {
        let gone = repo::files::unseen(&self.db, seen_before.timestamp_millis()).await?;
        if gone.is_empty() {
            return Ok(0);
        }
        let episodes = repo::episodes::ids_for_files(&self.db, &gone).await?;
        let shows = repo::episodes::show_ids(&self.db, &episodes).await?;
        for e in &episodes {
            repo::episodes::set_file(&self.db, *e, None, false).await?;
        }
        let n = repo::files::delete(&self.db, &gone).await?;
        for f in &gone {
            self.emit(entities::EPISODE_FILE, "deleted", *f);
        }
        for s in shows {
            self.recompute_statuses(s).await?;
        }
        Ok(n as u32)
    }

    /// Links a file to an episode found by automatic matching. Refused (false) when the
    /// episode has a hand-set link or the file is linked by hand elsewhere; a previous
    /// automatic link of the file moves to this episode.
    pub async fn link_file_auto(&self, episode_id: i64, file_id: i64) -> Result<bool> {
        let ep = repo::episodes::require(&self.db, episode_id).await?;
        if ep.manual_match || ep.file_id == Some(file_id) {
            return Ok(false);
        }
        repo::files::require(&self.db, file_id).await?;
        let mut shows: HashSet<i64> = HashSet::from([ep.show_id]);
        if let Some(other) = repo::episodes::for_file(&self.db, file_id).await? {
            if other.manual_match {
                return Ok(false);
            }
            repo::episodes::set_file(&self.db, other.id, None, false).await?;
            shows.insert(other.show_id);
        }
        repo::episodes::set_file(&self.db, episode_id, Some(file_id), false).await?;
        for s in shows {
            self.recompute_statuses(s).await?;
        }
        Ok(true)
    }

    /// `(show id, clean title, Audiobookshelf item id)` of every show. The clean title is
    /// compared without case, accents, punctuation or a leading "The" (see
    /// [`clean_title`](crate::library::clean_title)).
    pub async fn show_match_keys(&self) -> Result<Vec<(i64, String, Option<String>)>> {
        repo::shows::match_keys(&self.db).await
    }

    /// Every saved library file (for matchers).
    pub async fn episode_files(&self) -> Result<Vec<RadioEpisodeFileDto>> {
        repo::files::all(&self.db).await
    }

    /// Records a finished sync (shown by the review).
    pub async fn mark_library_synced(&self, at: DateTime<Utc>) -> Result<()> {
        self.db
            .settings()
            .set(keys::LIBRARY_SYNCED_AT, &at.timestamp_millis())
            .await?;
        Ok(())
    }

    /// Library podcasts no show covers (by `abs_item_id`, a matched file or the same clean
    /// title) and files matched to no episode.
    pub async fn library_review(&self) -> Result<RadioLibraryReviewDto> {
        let synced = self.db.settings().get::<i64>(keys::LIBRARY_SYNCED_AT).await?;
        let titles: HashSet<String> = repo::shows::match_keys(&self.db)
            .await?
            .into_iter()
            .map(|(_, key, _)| key)
            .filter(|k| !k.is_empty())
            .collect();
        let mut unmatched_shows = repo::files::unmatched_items(&self.db).await?;
        unmatched_shows.retain(|i| {
            !i.title
                .as_deref()
                .is_some_and(|t| titles.contains(&clean_title(t)))
        });
        Ok(RadioLibraryReviewDto {
            unmatched_shows,
            unmatched_files: repo::files::unmatched(&self.db).await?,
            synced_at: synced.map(from_ms),
        })
    }
}
