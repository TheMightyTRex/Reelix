//! Hand-set links between episodes and library files (`PUT`/`DELETE /radio/episodes/{id}/file`).
//!
//! A hand-set link (`manual_match`) is kept by automatic matching, and its file is not offered
//! to other episodes. Clearing a link by hand ("Not this file") also sets `manual_match`, with
//! no file, so automatic matching leaves the episode alone; clearing with `auto` hands the
//! episode back to automatic matching.

use std::collections::HashSet;

use reelix_api_types::radio::RadioEpisodeDto;

use crate::error::{RadioError, Result};
use crate::{RadioService, entities, repo};

impl RadioService {
    /// Links `file_id` to the episode by hand. A file linked automatically to another episode
    /// moves here; one linked by hand elsewhere is refused.
    pub async fn set_episode_file(&self, episode_id: i64, file_id: i64) -> Result<RadioEpisodeDto> {
        let ep = repo::episodes::require(&self.db, episode_id).await?;
        repo::files::get(&self.db, file_id)
            .await?
            .ok_or_else(|| RadioError::Validation(format!("library file {file_id} does not exist")))?;
        let mut shows: HashSet<i64> = HashSet::from([ep.show_id]);
        if let Some(other) = repo::episodes::for_file(&self.db, file_id).await?
            && other.id != episode_id
        {
            if other.manual_match {
                return Err(RadioError::Conflict(format!(
                    "that file is matched by hand to “{}”; clear that match first",
                    other.title
                )));
            }
            repo::episodes::set_file(&self.db, other.id, None, false).await?;
            shows.insert(other.show_id);
            self.emit(entities::EPISODE, "updated", other.id);
        }
        repo::episodes::set_file(&self.db, episode_id, Some(file_id), true).await?;
        for s in shows {
            self.recompute_statuses(s).await?;
        }
        self.emit(entities::EPISODE, "updated", episode_id);
        self.get_episode(episode_id).await
    }

    /// Clears the episode's file. `auto = false` ("Not this file") keeps it unmatched until
    /// changed by hand; `auto = true` ("Match automatically again") lets the next library sync
    /// match it. The status returns to Wanted or Not requested from the follow choice.
    pub async fn clear_episode_file(&self, episode_id: i64, auto: bool) -> Result<RadioEpisodeDto> {
        let ep = repo::episodes::require(&self.db, episode_id).await?;
        repo::episodes::set_file(&self.db, episode_id, None, !auto).await?;
        self.recompute_statuses(ep.show_id).await?;
        self.emit(entities::EPISODE, "updated", episode_id);
        self.get_episode(episode_id).await
    }
}
