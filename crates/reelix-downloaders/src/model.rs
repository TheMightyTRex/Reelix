//! Protocol-neutral types exchanged between the pipeline and download clients.

use reelix_core::MediaKind;
use serde::{Deserialize, Serialize};

/// Identifier the client uses for a download (SABnzbd `nzo_id`, NZBGet `NZBID`, torrent info hash, …).
/// Stored as `queue_items.download_id`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DownloadId(pub String);

impl DownloadId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for DownloadId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DownloadId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl std::fmt::Display for DownloadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What is handed to a client to start a download.
#[derive(Debug, Clone, PartialEq)]
pub enum ReleasePayload {
    /// `.torrent` file contents (`name` is used for the upload file name).
    TorrentFile {
        bytes: Vec<u8>,
        name: String,
    },
    Magnet(String),
    /// `.nzb` file contents.
    NzbFile {
        bytes: Vec<u8>,
        name: String,
    },
    /// A link the client (or the adapter) fetches itself.
    Url(String),
}

impl ReleasePayload {
    /// Short label for logs and errors.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::TorrentFile { .. } => "torrent file",
            Self::Magnet(_) => "magnet",
            Self::NzbFile { .. } => "nzb file",
            Self::Url(_) => "url",
        }
    }
}

/// Which payload form a client prefers, so the pipeline knows whether to fetch the file
/// through the indexer (keeping indexer credentials away from the client) or pass the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadPreference {
    File,
    Url,
}

/// Queue priority requested for a download. Clients map it to their own scale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Priority {
    /// Client default (category/global setting).
    #[default]
    Default,
    /// Add the job paused.
    Paused,
    Low,
    Normal,
    High,
    /// Start immediately, ignoring queue limits.
    Force,
}

impl Priority {
    pub const ALL: [Priority; 6] = [
        Self::Default,
        Self::Paused,
        Self::Low,
        Self::Normal,
        Self::High,
        Self::Force,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Paused => "Paused",
            Self::Low => "Low",
            Self::Normal => "Normal",
            Self::High => "High",
            Self::Force => "Force",
        }
    }

    /// Case-insensitive parse of [`Priority::as_str`] values.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

/// Per-grab options.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AddOptions {
    /// Overrides the category derived from `media_kind` and the client settings.
    pub category: Option<String>,
    /// Selects the movie or TV category configured on the client.
    pub media_kind: Option<MediaKind>,
    /// Overrides the recent/older priority from the client settings.
    pub priority: Option<Priority>,
    /// Released or aired within the last 14 days (selects `recent_priority`).
    pub is_recent: bool,
    pub add_paused: bool,
    /// Torrent only: stop seeding at this ratio.
    pub seed_ratio: Option<f64>,
    /// Torrent only: stop seeding after this many minutes.
    pub seed_time_minutes: Option<u64>,
    /// Release title; used as job name (`nzbname`) and file names.
    pub title: String,
}

/// Normalised download state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DownloadState {
    Queued,
    Downloading,
    Paused,
    /// Verifying / repairing / unpacking / moving. Never import in this state.
    PostProcessing,
    /// Finished; ready for import (torrents may still be seeding).
    Completed,
    Failed,
    /// Needs attention but is not definitively failed (stalled, tracker error, …).
    Warning,
}

/// Why a Usenet job failed, classified from the client's failure message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsenetFailure {
    MissingArticles,
    RepairFailed,
    UnpackFailed,
    PasswordProtected,
    DiskFull,
    TooLarge,
    FetchFailed,
    Duplicate,
    /// Unrecognised message; treated like a bad release.
    Other,
}

/// What the pipeline should do about a failed download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureAction {
    pub blocklist: bool,
    pub search_again: bool,
    /// Not the release's fault: raise a health warning instead.
    pub health_warning: bool,
    /// Reason stored on the blocklist entry.
    pub reason: &'static str,
}

impl UsenetFailure {
    /// Action table from the README SABnzbd spec.
    pub fn action(self) -> FailureAction {
        let bad = |reason| FailureAction {
            blocklist: true,
            search_again: true,
            health_warning: false,
            reason,
        };
        match self {
            Self::MissingArticles => bad("missing articles"),
            Self::RepairFailed => bad("repair failed"),
            Self::UnpackFailed => bad("unpacking failed"),
            Self::PasswordProtected => bad("password protected"),
            Self::FetchFailed => bad("nzb fetch failed"),
            Self::Duplicate => bad("duplicate rejected by client"),
            Self::Other => bad("download failed"),
            Self::DiskFull => FailureAction {
                blocklist: false,
                search_again: false,
                health_warning: true,
                reason: "out of disk space",
            },
            Self::TooLarge => FailureAction {
                blocklist: false,
                search_again: false,
                health_warning: true,
                reason: "aborted: job too large",
            },
        }
    }

    /// Classifies a free-text failure message (SABnzbd `fail_message`, NZBGet status text).
    pub fn classify(message: &str) -> Self {
        let m = message.to_ascii_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
        if has(&["password", "encrypt"]) {
            Self::PasswordProtected
        } else if has(&[
            "disk full",
            "disk is full",
            "disk space",
            "no space",
            "not enough space",
            "too little diskspace",
            "insufficient space",
        ]) {
            Self::DiskFull
        } else if has(&["too large", "exceeds", "size limit", "too big"]) {
            Self::TooLarge
        } else if has(&["duplicate"]) {
            Self::Duplicate
        } else if has(&[
            "fetch",
            "url",
            "download nzb",
            "invalid nzb",
            "empty nzb",
            "nzb file",
            "unable to retrieve",
        ]) {
            Self::FetchFailed
        } else if has(&["unpack", "unrar", "extract", "7z", "unzip"]) {
            Self::UnpackFailed
        } else if has(&["repair", "par2", "verification failed", "par "]) {
            Self::RepairFailed
        } else if has(&[
            "missing",
            "articles",
            "aborted",
            "incomplete",
            "health",
            "retention",
            "not complete",
        ]) {
            Self::MissingArticles
        } else {
            Self::Other
        }
    }
}

/// One download as reported by a client, already filtered to Reelix's categories.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadItem {
    pub download_id: DownloadId,
    pub title: String,
    pub category: Option<String>,
    pub state: DownloadState,
    pub total_bytes: u64,
    pub remaining_bytes: u64,
    pub eta_secs: Option<u64>,
    /// Path on the client's host (apply remote path mappings before use).
    pub output_path: Option<String>,
    /// Client status text (post-processing stage, error, tracker message…).
    pub message: Option<String>,
    /// Usenet failure classification when `state == Failed`.
    pub failure: Option<UsenetFailure>,
    pub priority: Option<Priority>,
    pub seed_ratio: Option<f64>,
    /// Seconds spent seeding.
    pub seeding_time: Option<u64>,
    /// Files may be moved instead of hardlinked/copied (seeding done or not a torrent).
    pub can_move_files: bool,
    /// The client entry may be removed after import.
    pub can_be_removed: bool,
}

impl DownloadItem {
    /// Minimal item; adapters fill in the rest.
    pub fn new(id: impl Into<DownloadId>, title: impl Into<String>, state: DownloadState) -> Self {
        Self {
            download_id: id.into(),
            title: title.into(),
            category: None,
            state,
            total_bytes: 0,
            remaining_bytes: 0,
            eta_secs: None,
            output_path: None,
            message: None,
            failure: None,
            priority: None,
            seed_ratio: None,
            seeding_time: None,
            can_move_files: false,
            can_be_removed: false,
        }
    }

    /// Progress in `0.0..=1.0`.
    pub fn progress(&self) -> f64 {
        if self.total_bytes == 0 {
            return if self.state == DownloadState::Completed {
                1.0
            } else {
                0.0
            };
        }
        1.0 - (self.remaining_bytes.min(self.total_bytes) as f64 / self.total_bytes as f64)
    }
}

/// Kind of a client health issue (shown on System → Health).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthKind {
    Unreachable,
    AuthFailed,
    /// Usenet: only the NZB key was given.
    NzbKeyOnly,
    VersionTooOld,
    CategoryMissing,
    /// The client renames/sorts files for a Reelix category.
    SortingEnabled,
    /// Completed jobs vanish from history before Reelix sees them.
    HistoryRetention,
    /// A category's completed folder is not reachable locally.
    OutputDirUnreachable,
    /// Server/disk warning reported by the client.
    ClientWarning,
    /// Torrents get removed when their seed ratio is reached.
    RemovesOnRatio,
    /// Configuration that will break tracking (e.g. a disabled plugin).
    Misconfigured,
}

/// A single health finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthWarning {
    pub kind: HealthKind,
    pub message: String,
    /// Path in the client's web UI with the relevant setting (relative to its base URL).
    pub settings_path: Option<String>,
}

impl HealthWarning {
    pub fn new(kind: HealthKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            settings_path: None,
        }
    }

    pub fn at(mut self, path: &str) -> Self {
        self.settings_path = Some(path.into());
        self
    }
}

/// Where a client puts finished downloads for one category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDir {
    /// `None` for the client's default directory.
    pub category: Option<String>,
    /// Remote path (client host).
    pub path: String,
}

/// Result of a status/health poll.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientStatus {
    pub version: Option<String>,
    pub output_dirs: Vec<OutputDir>,
    pub warnings: Vec<HealthWarning>,
}

impl ClientStatus {
    /// Adds an [`HealthKind::OutputDirUnreachable`] warning for every output folder that,
    /// after `map_remote` (remote path mappings; `None` = no mapping, use as-is), does not
    /// exist locally.
    pub fn check_output_dirs(&mut self, map_remote: impl Fn(&str) -> Option<std::path::PathBuf>) {
        for d in &self.output_dirs {
            let local = map_remote(&d.path).unwrap_or_else(|| std::path::PathBuf::from(&d.path));
            if !local.is_dir() {
                let what = d
                    .category
                    .as_deref()
                    .map_or_else(|| "default".to_owned(), |c| format!("category `{c}`"));
                self.warnings.push(HealthWarning::new(
                    HealthKind::OutputDirUnreachable,
                    format!(
                        "Completed folder for {what} ({}) is not reachable locally ({}); add a remote path mapping",
                        d.path,
                        local.display()
                    ),
                ));
            }
        }
    }
}

/// One step of a connection test (connect, auth, version, categories…).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestCheck {
    pub name: String,
    pub ok: bool,
    pub message: Option<String>,
}

/// Per-check outcome of [`crate::DownloadClient::test_report`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TestReport {
    pub checks: Vec<TestCheck>,
}

impl TestReport {
    pub fn pass(&mut self, name: &str) {
        self.checks.push(TestCheck {
            name: name.into(),
            ok: true,
            message: None,
        });
    }

    pub fn fail(&mut self, name: &str, message: impl Into<String>) {
        self.checks.push(TestCheck {
            name: name.into(),
            ok: false,
            message: Some(message.into()),
        });
    }

    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    /// First failure message, if any.
    pub fn first_failure(&self) -> Option<String> {
        self.checks
            .iter()
            .find(|c| !c.ok)
            .map(|c| format!("{}: {}", c.name, c.message.as_deref().unwrap_or("check failed")))
    }
}

/// Settings every client shares, used by the pipeline when picking and cleaning up clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub implementation: String,
    pub name: String,
    /// Lower wins when several clients of the same protocol match.
    pub priority: u8,
    pub remove_completed: bool,
    pub remove_failed: bool,
    pub tags: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_classification_table() {
        use UsenetFailure::*;
        let cases = [
            ("Download failed - Out of server retention", MissingArticles),
            ("Something odd happened", Other),
            (
                "Aborted, cannot be completed - https://sabnzbd.org/not-complete",
                MissingArticles,
            ),
            ("Repair failed, not enough repair blocks (52 short)", RepairFailed),
            ("Unpacking failed, archive requires a password", PasswordProtected),
            ("Unpacking failed, write error or disk is full?", DiskFull),
            ("Aborted, encryption detected", PasswordProtected),
            ("Unpacking failed, CRC error", UnpackFailed),
            ("URL Fetching failed; Server returned 404", FetchFailed),
            ("Duplicate NZB", Duplicate),
            ("Too little diskspace", DiskFull),
            ("Aborted, job is too large", TooLarge),
            ("Aborted, cannot be completed (missing articles)", MissingArticles),
        ];
        for (msg, want) in cases {
            assert_eq!(UsenetFailure::classify(msg), want, "{msg}");
        }
    }

    #[test]
    fn failure_actions() {
        assert!(UsenetFailure::MissingArticles.action().blocklist);
        assert_eq!(
            UsenetFailure::PasswordProtected.action().reason,
            "password protected"
        );
        let disk = UsenetFailure::DiskFull.action();
        assert!(!disk.blocklist && disk.health_warning);
        assert!(!UsenetFailure::TooLarge.action().blocklist);
        assert!(UsenetFailure::Duplicate.action().search_again);
    }

    #[test]
    fn progress_and_priority() {
        let mut i = DownloadItem::new("x", "t", DownloadState::Downloading);
        i.total_bytes = 200;
        i.remaining_bytes = 50;
        assert!((i.progress() - 0.75).abs() < 1e-9);
        assert_eq!(Priority::parse("force"), Some(Priority::Force));
        assert_eq!(Priority::parse("nope"), None);
    }
}
