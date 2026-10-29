//! SABnzbd API payloads and the status mapping table.
//!
//! SABnzbd encodes most numbers as strings (`"mb": "1277.65"`), so fields are decoded
//! leniently via [`lenient`].

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::model::{DownloadItem, DownloadState, Priority, UsenetFailure};

/// Generic `{"status": false, "error": "…"}` envelope.
#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    #[serde(default)]
    pub status: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VersionResponse {
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct AddResponse {
    #[serde(default)]
    pub status: bool,
    #[serde(default)]
    pub nzo_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CategoriesResponse {
    pub categories: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConfigResponse {
    pub config: Value,
}

#[derive(Debug, Deserialize)]
pub struct QueueResponse {
    pub queue: Queue,
}

#[derive(Debug, Default, Deserialize)]
pub struct Queue {
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub slots: Vec<QueueSlot>,
}

#[derive(Debug, Deserialize)]
pub struct QueueSlot {
    pub nzo_id: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub cat: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub mb: f64,
    #[serde(default, deserialize_with = "lenient")]
    pub mbleft: f64,
    #[serde(default)]
    pub timeleft: String,
    #[serde(default)]
    pub priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryResponse {
    pub history: History,
}

#[derive(Debug, Default, Deserialize)]
pub struct History {
    #[serde(default)]
    pub slots: Vec<HistorySlot>,
}

#[derive(Debug, Deserialize)]
pub struct HistorySlot {
    pub nzo_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub bytes: f64,
    #[serde(default)]
    pub storage: Option<String>,
    #[serde(default)]
    pub fail_message: String,
    #[serde(default)]
    pub action_line: String,
}

/// Number or numeric string → f64 (garbage → 0).
pub fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    })
}

const MIB: f64 = 1024.0 * 1024.0;

/// Queue status → state (README "Status Mapping", queue rows).
pub fn queue_state(status: &str, queue_paused: bool) -> DownloadState {
    match status {
        "Paused" => DownloadState::Paused,
        _ if queue_paused => DownloadState::Paused,
        "Downloading" => DownloadState::Downloading,
        "Queued" | "Grabbing" | "Fetching" | "Propagating" | "Idle" => DownloadState::Queued,
        // Post-processing stages can briefly show on queue slots too.
        "Checking" | "QuickCheck" | "Verifying" | "Repairing" | "Extracting" | "Moving" | "Running" => {
            DownloadState::PostProcessing
        }
        _ => DownloadState::Queued,
    }
}

/// History status → state (README "Status Mapping", history rows).
pub fn history_state(status: &str) -> DownloadState {
    match status {
        "Completed" => DownloadState::Completed,
        "Failed" => DownloadState::Failed,
        // `Queued` in history means waiting for post-processing.
        "Queued" | "Checking" | "QuickCheck" | "Verifying" | "Repairing" | "Extracting" | "Moving"
        | "Running" | "Fetching" => DownloadState::PostProcessing,
        _ => DownloadState::Warning,
    }
}

/// `"0:16:44"` or `"1:02:03:04"` (days first) → seconds.
pub fn parse_timeleft(s: &str) -> Option<u64> {
    let parts: Vec<u64> = s
        .trim()
        .split(':')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    let mul = [86_400, 3_600, 60, 1];
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let off = 4 - parts.len();
    Some(parts.iter().enumerate().map(|(i, v)| v * mul[i + off]).sum())
}

/// SABnzbd priority number for a Reelix priority.
pub fn priority_value(p: Priority) -> i32 {
    match p {
        Priority::Default => -100,
        Priority::Paused => -2,
        Priority::Low => -1,
        Priority::Normal => 0,
        Priority::High => 1,
        Priority::Force => 2,
    }
}

impl QueueSlot {
    pub fn into_item(self, queue_paused: bool) -> DownloadItem {
        let state = queue_state(&self.status, queue_paused);
        let mut item = DownloadItem::new(self.nzo_id, self.filename, state);
        item.category = self.cat;
        item.total_bytes = (self.mb * MIB).round() as u64;
        item.remaining_bytes = (self.mbleft * MIB).round() as u64;
        item.eta_secs = parse_timeleft(&self.timeleft).filter(|s| *s > 0);
        item.priority = self.priority.as_deref().and_then(Priority::parse);
        item.message = match self.status.as_str() {
            "Grabbing" | "Fetching" => Some("Fetching NZB".into()),
            "Propagating" => Some("Waiting for propagation delay".into()),
            "Downloading" | "Queued" | "Paused" | "" => None,
            other => Some(other.into()),
        };
        item
    }
}

impl HistorySlot {
    pub fn into_item(self) -> DownloadItem {
        let state = history_state(&self.status);
        let mut item = DownloadItem::new(self.nzo_id, self.name, state);
        item.category = self.category;
        item.total_bytes = self.bytes.max(0.0) as u64;
        match state {
            DownloadState::Completed => {
                item.output_path = self.storage.filter(|s| !s.is_empty());
                item.can_move_files = true;
                item.can_be_removed = true;
            }
            DownloadState::Failed => {
                let msg = if self.fail_message.is_empty() {
                    "Download failed".to_owned()
                } else {
                    self.fail_message
                };
                item.failure = Some(UsenetFailure::classify(&msg));
                item.message = Some(msg);
                item.can_be_removed = true;
            }
            _ => {
                // Show the post-processing stage ("Repairing", "Extracting", …).
                item.remaining_bytes = 0;
                item.message = Some(if self.action_line.is_empty() {
                    self.status
                } else {
                    format!("{}: {}", self.status, self.action_line)
                });
            }
        }
        item
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_mapping_table() {
        use DownloadState::*;
        for (s, want) in [
            ("Queued", Queued),
            ("Grabbing", Queued),
            ("Fetching", Queued),
            ("Propagating", Queued),
            ("Downloading", Downloading),
            ("Paused", Paused),
            ("Checking", PostProcessing),
        ] {
            assert_eq!(queue_state(s, false), want, "{s}");
        }
        assert_eq!(queue_state("Downloading", true), Paused);
    }

    #[test]
    fn history_mapping_table() {
        use DownloadState::*;
        for (s, want) in [
            ("Checking", PostProcessing),
            ("QuickCheck", PostProcessing),
            ("Verifying", PostProcessing),
            ("Repairing", PostProcessing),
            ("Extracting", PostProcessing),
            ("Moving", PostProcessing),
            ("Running", PostProcessing),
            ("Queued", PostProcessing),
            ("Completed", Completed),
            ("Failed", Failed),
            ("Weird", Warning),
        ] {
            assert_eq!(history_state(s), want, "{s}");
        }
    }

    #[test]
    fn timeleft_and_priority() {
        assert_eq!(parse_timeleft("0:16:44"), Some(1004));
        assert_eq!(parse_timeleft("1:00:00:01"), Some(86_401));
        assert_eq!(parse_timeleft("x"), None);
        let v: Vec<i32> = Priority::ALL.into_iter().map(priority_value).collect();
        assert_eq!(v, [-100, -2, -1, 0, 1, 2]);
    }
}
