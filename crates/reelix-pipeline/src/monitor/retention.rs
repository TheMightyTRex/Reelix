//! History clean-up: delete history before a date, and how many days to keep (the daily
//! `prune_history` task removes anything older).

use chrono::{DateTime, Utc};
use reelix_api_types::pipeline::HistorySettings;
use reelix_db::now_ms;

use super::{ActivityError, ActivityService, Result};
use crate::store::history;

/// Settings key of [`HistorySettings`].
pub const HISTORY_SETTINGS_KEY: &str = "history_retention";

const DAY_MS: i64 = 86_400_000;

impl ActivityService {
    /// `DELETE /history?before=`: removes every history row dated before `before`.
    pub async fn delete_history_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let removed = history::prune(&self.db, before.timestamp_millis()).await?;
        if removed > 0 {
            self.emit("history", "deleted", 0);
        }
        Ok(removed)
    }

    /// How many days of history to keep (90 until it is changed).
    pub async fn history_settings(&self) -> Result<HistorySettings> {
        Ok(self
            .db
            .settings()
            .get::<HistorySettings>(HISTORY_SETTINGS_KEY)
            .await?
            .unwrap_or_default())
    }

    pub async fn set_history_settings(&self, settings: HistorySettings) -> Result<HistorySettings> {
        if settings.keep_days > HistorySettings::MAX_KEEP_DAYS {
            return Err(ActivityError::validation(format!(
                "keep_days must be between 0 and {}",
                HistorySettings::MAX_KEEP_DAYS
            )));
        }
        self.db.settings().set(HISTORY_SETTINGS_KEY, &settings).await?;
        Ok(settings)
    }

    /// Daily task: removes history older than the days to keep; 0 keeps everything.
    pub async fn prune_history(&self) -> Result<u64> {
        let keep_days = self.history_settings().await?.keep_days;
        if keep_days == 0 {
            return Ok(0);
        }
        let cutoff = now_ms() - i64::from(keep_days) * DAY_MS;
        let removed = history::prune(&self.db, cutoff).await?;
        if removed > 0 {
            self.emit("history", "deleted", 0);
        }
        Ok(removed)
    }
}
