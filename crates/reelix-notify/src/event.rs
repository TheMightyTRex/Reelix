//! Notification events and their payloads.
//!
//! Payloads are self-contained snapshots (titles, ids, paths) so a notifier never needs
//! to touch the database. The serialized form (`{"event": "...", "data": {...}}`) is part
//! of the documented webhook contract — add fields, never rename them.

use std::fmt;

use reelix_core::{ContentKind, MediaKind, Protocol};
use serde::{Deserialize, Serialize};

/// Everything a notifier can be asked to deliver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum NotificationEvent {
    /// Sent by the "Test" button.
    Test {
        message: String,
    },
    /// A release was sent to a download client.
    Grab {
        media: MediaSummary,
        release: ReleaseSummary,
    },
    /// A file was imported into the library.
    Import {
        media: MediaSummary,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release: Option<ReleaseSummary>,
        file: FileSummary,
        is_upgrade: bool,
        /// Files replaced by this import (upgrades).
        #[serde(default)]
        replaced_files: Vec<FileSummary>,
    },
    /// Files were renamed on disk.
    Rename {
        media: MediaSummary,
        files: Vec<RenamedFile>,
    },
    MediaAdded {
        media: MediaSummary,
    },
    MediaDeleted {
        media: MediaSummary,
        files_deleted: bool,
    },
    FileDeleted {
        media: MediaSummary,
        file: FileSummary,
        reason: DeleteReason,
    },
    HealthIssue(HealthSummary),
    HealthRestored(HealthSummary),
    /// A download finished but could not be imported automatically.
    ManualInteractionRequired {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media: Option<MediaSummary>,
        release: ReleaseSummary,
        message: String,
    },
    RequestPending {
        media: MediaSummary,
        request: RequestSummary,
    },
    RequestApproved {
        media: MediaSummary,
        request: RequestSummary,
    },
    RequestDeclined {
        media: MediaSummary,
        request: RequestSummary,
    },
    RequestAvailable {
        media: MediaSummary,
        request: RequestSummary,
    },
    IssueReported {
        media: MediaSummary,
        issue: IssueSummary,
    },
    IssueResolved {
        media: MediaSummary,
        issue: IssueSummary,
    },
    ApplicationUpdate(UpdateSummary),
}

/// A movie or series (optionally narrowed to episodes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaSummary {
    pub kind: MediaKind,
    /// Reelix library id, when the media is in the library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb_id: Option<i64>,
    /// Absolute poster URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(default)]
    pub episodes: Vec<EpisodeSummary>,
    /// Set for books/music (then `kind` is a placeholder `Movie`); renderers prefer it when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentKind>,
}

impl MediaSummary {
    /// Minimal summary with only kind and title.
    pub fn new(kind: MediaKind, title: impl Into<String>) -> Self {
        Self {
            kind,
            id: None,
            title: title.into(),
            year: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            poster_url: None,
            overview: None,
            episodes: Vec::new(),
            content: None,
        }
    }

    /// Minimal summary of any content kind. Books and music get the placeholder `kind` Movie
    /// and carry the real kind in `content`.
    pub fn content(kind: ContentKind, title: impl Into<String>) -> Self {
        Self {
            content: kind.media_kind().is_none().then_some(kind),
            ..Self::new(kind.media_kind().unwrap_or(MediaKind::Movie), title)
        }
    }

    /// `Title (Year)`, plus `- S01E02` style episode labels for series.
    pub fn display_title(&self) -> String {
        let mut s = match self.year {
            Some(y) => format!("{} ({y})", self.title),
            None => self.title.clone(),
        };
        if !self.episodes.is_empty() {
            let eps: Vec<String> = self.episodes.iter().map(EpisodeSummary::label).collect();
            s.push_str(" - ");
            s.push_str(&eps.join(", "));
        }
        s
    }

    /// A public link for the media (TMDB, falling back to IMDb).
    pub fn link(&self) -> Option<String> {
        let kind = match self.kind {
            MediaKind::Movie => "movie",
            MediaKind::Series => "tv",
        };
        self.tmdb_id
            .map(|id| format!("https://www.themoviedb.org/{kind}/{id}"))
            .or_else(|| {
                self.imdb_id
                    .as_ref()
                    .map(|id| format!("https://www.imdb.com/title/{id}/"))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeSummary {
    pub season: u32,
    pub episode: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air_date: Option<String>,
}

impl EpisodeSummary {
    /// `S01E02`.
    pub fn label(&self) -> String {
        format!("S{:02}E{:02}", self.season, self.episode)
    }
}

/// A release that was grabbed or is waiting for manual import.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReleaseSummary {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Display quality, e.g. `Bluray-1080p`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Protocol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_group: Option<String>,
    #[serde(default)]
    pub custom_formats: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_format_score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_id: Option<String>,
}

/// A file in the library.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FileSummary {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenamedFile {
    pub previous_path: String,
    pub new_path: String,
}

/// Why a file was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteReason {
    Manual,
    Upgrade,
    MissingFromDisk,
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthLevel {
    Notice,
    Warning,
    Error,
}

/// A health check result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthSummary {
    /// Check that produced it, e.g. `DownloadClientCheck`.
    pub source: String,
    pub level: HealthLevel,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Approved,
    Declined,
    Available,
}

/// A user request (Jellyseerr-style).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestSummary {
    pub id: i64,
    pub status: RequestStatus,
    pub requested_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by_email: Option<String>,
    /// Requested seasons (empty for movies or "all seasons").
    #[serde(default)]
    pub seasons: Vec<u32>,
    #[serde(default)]
    pub is_4k: bool,
    /// Who approved/declined it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_by: Option<String>,
    /// Decline reason or admin comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Audio,
    Video,
    Subtitles,
    Other,
}

impl fmt::Display for IssueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Subtitles => "Subtitles",
            Self::Other => "Other",
        })
    }
}

/// A user-reported issue on available media.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueSummary {
    pub id: i64,
    pub issue_type: IssueType,
    pub message: String,
    pub reported_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// A new Reelix version was installed or is available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateSummary {
    pub previous_version: String,
    pub new_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_notes_url: Option<String>,
    #[serde(default)]
    pub changes: Vec<String>,
}

/// Discriminant of [`NotificationEvent`], used for subscriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Test,
    Grab,
    Import,
    Upgrade,
    Rename,
    MediaAdded,
    MediaDeleted,
    FileDeleted,
    HealthIssue,
    HealthRestored,
    ManualInteractionRequired,
    RequestPending,
    RequestApproved,
    RequestDeclined,
    RequestAvailable,
    IssueReported,
    IssueResolved,
    ApplicationUpdate,
}

impl EventKind {
    /// Every kind, in display order.
    pub const ALL: [EventKind; 18] = [
        Self::Test,
        Self::Grab,
        Self::Import,
        Self::Upgrade,
        Self::Rename,
        Self::MediaAdded,
        Self::MediaDeleted,
        Self::FileDeleted,
        Self::HealthIssue,
        Self::HealthRestored,
        Self::ManualInteractionRequired,
        Self::RequestPending,
        Self::RequestApproved,
        Self::RequestDeclined,
        Self::RequestAvailable,
        Self::IssueReported,
        Self::IssueResolved,
        Self::ApplicationUpdate,
    ];

    /// Stable snake_case id (same as the serde name).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Grab => "grab",
            Self::Import => "import",
            Self::Upgrade => "upgrade",
            Self::Rename => "rename",
            Self::MediaAdded => "media_added",
            Self::MediaDeleted => "media_deleted",
            Self::FileDeleted => "file_deleted",
            Self::HealthIssue => "health_issue",
            Self::HealthRestored => "health_restored",
            Self::ManualInteractionRequired => "manual_interaction_required",
            Self::RequestPending => "request_pending",
            Self::RequestApproved => "request_approved",
            Self::RequestDeclined => "request_declined",
            Self::RequestAvailable => "request_available",
            Self::IssueReported => "issue_reported",
            Self::IssueResolved => "issue_resolved",
            Self::ApplicationUpdate => "application_update",
        }
    }

    /// Human label for settings forms.
    pub fn label(self) -> &'static str {
        match self {
            Self::Test => "Test",
            Self::Grab => "On grab",
            Self::Import => "On import",
            Self::Upgrade => "On upgrade",
            Self::Rename => "On rename",
            Self::MediaAdded => "On media added",
            Self::MediaDeleted => "On media deleted",
            Self::FileDeleted => "On file deleted",
            Self::HealthIssue => "On health issue",
            Self::HealthRestored => "On health restored",
            Self::ManualInteractionRequired => "On manual interaction required",
            Self::RequestPending => "On request pending approval",
            Self::RequestApproved => "On request approved",
            Self::RequestDeclined => "On request declined",
            Self::RequestAvailable => "On request available",
            Self::IssueReported => "On issue reported",
            Self::IssueResolved => "On issue resolved",
            Self::ApplicationUpdate => "On application update",
        }
    }

    /// Parses [`EventKind::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

impl NotificationEvent {
    /// The subscription kind; imports that replaced a file count as [`EventKind::Upgrade`].
    pub fn kind(&self) -> EventKind {
        match self {
            Self::Test { .. } => EventKind::Test,
            Self::Grab { .. } => EventKind::Grab,
            Self::Import { is_upgrade, .. } => {
                if *is_upgrade {
                    EventKind::Upgrade
                } else {
                    EventKind::Import
                }
            }
            Self::Rename { .. } => EventKind::Rename,
            Self::MediaAdded { .. } => EventKind::MediaAdded,
            Self::MediaDeleted { .. } => EventKind::MediaDeleted,
            Self::FileDeleted { .. } => EventKind::FileDeleted,
            Self::HealthIssue(_) => EventKind::HealthIssue,
            Self::HealthRestored(_) => EventKind::HealthRestored,
            Self::ManualInteractionRequired { .. } => EventKind::ManualInteractionRequired,
            Self::RequestPending { .. } => EventKind::RequestPending,
            Self::RequestApproved { .. } => EventKind::RequestApproved,
            Self::RequestDeclined { .. } => EventKind::RequestDeclined,
            Self::RequestAvailable { .. } => EventKind::RequestAvailable,
            Self::IssueReported { .. } => EventKind::IssueReported,
            Self::IssueResolved { .. } => EventKind::IssueResolved,
            Self::ApplicationUpdate(_) => EventKind::ApplicationUpdate,
        }
    }

    /// The media the event is about, if any.
    pub fn media(&self) -> Option<&MediaSummary> {
        match self {
            Self::Grab { media, .. }
            | Self::Import { media, .. }
            | Self::Rename { media, .. }
            | Self::MediaAdded { media }
            | Self::MediaDeleted { media, .. }
            | Self::FileDeleted { media, .. }
            | Self::RequestPending { media, .. }
            | Self::RequestApproved { media, .. }
            | Self::RequestDeclined { media, .. }
            | Self::RequestAvailable { media, .. }
            | Self::IssueReported { media, .. }
            | Self::IssueResolved { media, .. } => Some(media),
            Self::ManualInteractionRequired { media, .. } => media.as_ref(),
            Self::Test { .. }
            | Self::HealthIssue(_)
            | Self::HealthRestored(_)
            | Self::ApplicationUpdate(_) => None,
        }
    }

    /// The default test event.
    pub fn test() -> Self {
        Self::Test {
            message: "This is a test notification from Reelix.".into(),
        }
    }
}

/// A set of [`EventKind`]s. Serialized as a list of kind ids.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EventMask(u32);

impl EventMask {
    pub const NONE: EventMask = EventMask(0);

    /// Every event kind.
    pub fn all() -> Self {
        Self::from_kinds(EventKind::ALL)
    }

    pub fn from_kinds(kinds: impl IntoIterator<Item = EventKind>) -> Self {
        Self(kinds.into_iter().fold(0, |acc, k| acc | k.bit()))
    }

    pub fn contains(self, kind: EventKind) -> bool {
        self.0 & kind.bit() != 0
    }

    pub fn insert(&mut self, kind: EventKind) {
        self.0 |= kind.bit();
    }

    pub fn remove(&mut self, kind: EventKind) {
        self.0 &= !kind.bit();
    }

    pub fn intersect(self, other: EventMask) -> EventMask {
        EventMask(self.0 & other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = EventKind> {
        EventKind::ALL.into_iter().filter(move |k| self.contains(*k))
    }
}

impl FromIterator<EventKind> for EventMask {
    fn from_iter<T: IntoIterator<Item = EventKind>>(iter: T) -> Self {
        Self::from_kinds(iter)
    }
}

impl fmt::Debug for EventMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl Serialize for EventMask {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for EventMask {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Vec::<EventKind>::deserialize(d)?.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_roundtrip_and_contains() {
        let m = EventMask::from_kinds([EventKind::Grab, EventKind::Upgrade]);
        assert!(m.contains(EventKind::Grab));
        assert!(!m.contains(EventKind::Import));
        let json = serde_json::to_value(m).unwrap();
        assert_eq!(json, serde_json::json!(["grab", "upgrade"]));
        assert_eq!(serde_json::from_value::<EventMask>(json).unwrap(), m);
        assert_eq!(EventMask::all().iter().count(), EventKind::ALL.len());
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
    }

    #[test]
    fn import_upgrade_kind() {
        let ev = NotificationEvent::Import {
            media: MediaSummary::new(MediaKind::Movie, "Dune"),
            release: None,
            file: FileSummary::default(),
            is_upgrade: true,
            replaced_files: vec![],
        };
        assert_eq!(ev.kind(), EventKind::Upgrade);
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["event"], "import");
        assert_eq!(json["data"]["is_upgrade"], true);
    }

    #[test]
    fn display_title_with_episodes() {
        let mut m = MediaSummary::new(MediaKind::Series, "Severance");
        m.year = Some(2022);
        m.episodes = vec![
            EpisodeSummary {
                season: 2,
                episode: 1,
                absolute: None,
                title: None,
                air_date: None,
            },
            EpisodeSummary {
                season: 2,
                episode: 2,
                absolute: None,
                title: None,
                air_date: None,
            },
        ];
        assert_eq!(m.display_title(), "Severance (2022) - S02E01, S02E02");
        m.tmdb_id = Some(95396);
        assert_eq!(m.link().unwrap(), "https://www.themoviedb.org/tv/95396");
    }
}
