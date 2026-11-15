//! Stalled downloads (DASH-407): a download that makes no progress for the set time while
//! downloading is flagged in the queue, or removed, blocklisted and searched for again.
//!
//! Progress is noted in memory at each poll (bytes left and since when), so a restart starts
//! the clock again. Paused and queued downloads do not count as stalled.

use reelix_api_types::pipeline::{StalledAction, StalledSettings};
use reelix_downloaders::{DownloadItem, DownloadState, FailureAction};

use super::{ActivityError, ActivityService, ClientHandle, Result, lock};
use crate::store::tracked::TrackedDownload;

/// Settings key of [`StalledSettings`].
pub const STALLED_SETTINGS_KEY: &str = "stalled_downloads";

const HOUR_MS: i64 = 3_600_000;

impl ActivityService {
    /// The stalled download settings (6 hours, flag only, until changed).
    pub async fn stalled_settings(&self) -> Result<StalledSettings> {
        Ok(self
            .db
            .settings()
            .get::<StalledSettings>(STALLED_SETTINGS_KEY)
            .await?
            .unwrap_or_default())
    }

    pub async fn set_stalled_settings(&self, settings: StalledSettings) -> Result<StalledSettings> {
        if settings.stalled_hours > StalledSettings::MAX_HOURS {
            return Err(ActivityError::validation(format!(
                "stalled_hours must be between 0 and {}",
                StalledSettings::MAX_HOURS
            )));
        }
        self.db.settings().set(STALLED_SETTINGS_KEY, &settings).await?;
        Ok(settings)
    }

    /// Moves the stall clock on (tests only: stalls take hours).
    #[doc(hidden)]
    pub fn advance_stall_clock(&self, ms: i64) {
        self.stall_clock
            .fetch_add(ms, std::sync::atomic::Ordering::Relaxed);
    }

    fn stall_now(&self) -> i64 {
        reelix_db::now_ms() + self.stall_clock.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Notes `item`'s progress. When it has made none for the set time, adds the stalled
    /// message to `messages` (flag), or fails the download, blocklists the release, searches
    /// again and removes it from the client (remove and search). Returns true when it was
    /// removed, so the caller leaves it alone.
    pub(crate) async fn check_stalled(
        &self,
        t: &TrackedDownload,
        h: &ClientHandle,
        item: &DownloadItem,
        messages: &mut Vec<String>,
    ) -> Result<bool> {
        let now = self.stall_now();
        let since = {
            let mut progress = lock(&self.progress);
            if !matches!(item.state, DownloadState::Downloading | DownloadState::Warning) {
                progress.remove(&t.id);
                return Ok(false);
            }
            let entry = progress.entry(t.id).or_insert((item.remaining_bytes, now));
            if entry.0 != item.remaining_bytes {
                *entry = (item.remaining_bytes, now);
            }
            entry.1
        };
        // The shortest setting is an hour, so the settings are read only after that.
        if now - since < HOUR_MS {
            return Ok(false);
        }
        let settings = self.stalled_settings().await?;
        if settings.stalled_hours == 0 || now - since < i64::from(settings.stalled_hours) * HOUR_MS {
            return Ok(false);
        }
        let hours = settings.stalled_hours;
        let unit = if hours == 1 { "hour" } else { "hours" };
        match settings.action {
            StalledAction::Flag => {
                messages.push(format!(
                    "Stalled: no progress for {hours} {unit} or more. Remove it to search for another \
                     release, or wait if it is a rare one."
                ));
                Ok(false)
            }
            StalledAction::RemoveAndSearch => {
                lock(&self.progress).remove(&t.id);
                let message = format!(
                    "Stalled: no progress for {hours} {unit}, so it was removed and another release is \
                     being searched for"
                );
                let action = FailureAction {
                    blocklist: true,
                    search_again: true,
                    health_warning: false,
                    reason: "stalled",
                };
                self.handle_failure(t, &message, action, Some(h), false).await?;
                if !h.client.info().remove_failed {
                    self.remove_from_client(h, &item.download_id, true).await;
                }
                Ok(true)
            }
        }
    }
}
