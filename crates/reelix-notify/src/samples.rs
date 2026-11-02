//! Sample events for every [`EventKind`](crate::EventKind): used by tests and UI previews.

use reelix_core::{MediaKind, Protocol};

use crate::event::*;

fn movie() -> MediaSummary {
    MediaSummary {
        kind: MediaKind::Movie,
        id: Some(42),
        title: "The Lighthouse Keeper".into(),
        year: Some(2023),
        tmdb_id: Some(912_345),
        imdb_id: Some("tt9123450".into()),
        tvdb_id: None,
        poster_url: Some("https://image.tmdb.org/t/p/w500/lighthouse.jpg".into()),
        overview: Some("A keeper on a remote island begins to doubt the tides.".into()),
        episodes: vec![],
        content: None,
        genres: vec!["Drama".into(), "Mystery".into(), "Thriller".into()],
        rating: Some(7.4),
        backdrop_url: Some("https://image.tmdb.org/t/p/w1280/lighthouse-backdrop.jpg".into()),
        trailer_url: Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".into()),
    }
}

fn series() -> MediaSummary {
    MediaSummary {
        kind: MediaKind::Series,
        id: Some(7),
        title: "Harbor Lights".into(),
        year: Some(2021),
        tmdb_id: Some(120_001),
        imdb_id: Some("tt1200010".into()),
        tvdb_id: Some(380_001),
        poster_url: None,
        overview: None,
        episodes: vec![EpisodeSummary {
            season: 2,
            episode: 5,
            absolute: None,
            title: Some("Low Tide".into()),
            air_date: Some("2024-03-14".into()),
        }],
        content: None,
        genres: vec!["Drama".into()],
        rating: Some(8.1),
        backdrop_url: None,
        trailer_url: None,
    }
}

fn release() -> ReleaseSummary {
    ReleaseSummary {
        title: "The.Lighthouse.Keeper.2023.1080p.BluRay.x264-NOCTURN".into(),
        indexer: Some("Example Torznab".into()),
        size_bytes: Some(9_663_676_416),
        quality: Some("Bluray-1080p".into()),
        protocol: Some(Protocol::Torrent),
        release_group: Some("NOCTURN".into()),
        custom_formats: vec!["HD Bluray Tier 01".into()],
        custom_format_score: Some(1750),
        download_client: Some("qBittorrent".into()),
        download_id: Some("8f1c0e9a2b7d4c3e".into()),
    }
}

fn file() -> FileSummary {
    FileSummary {
        path: "/movies/The Lighthouse Keeper (2023)/The Lighthouse Keeper (2023) Bluray-1080p.mkv".into(),
        size_bytes: Some(9_663_676_416),
        quality: Some("Bluray-1080p".into()),
        release_group: Some("NOCTURN".into()),
        scene_name: Some("The.Lighthouse.Keeper.2023.1080p.BluRay.x264-NOCTURN".into()),
        video_codec: Some("h264".into()),
        audio_codec: Some("DTS-HD MA 5.1".into()),
        audio_languages: vec!["English".into()],
        subtitle_languages: vec!["English".into(), "Spanish".into()],
    }
}

fn request(status: RequestStatus) -> RequestSummary {
    RequestSummary {
        id: 31,
        status,
        requested_by: "alice".into(),
        requested_by_email: Some("alice@example.com".into()),
        seasons: vec![1, 2],
        is_4k: false,
        modified_by: (status != RequestStatus::Pending).then(|| "admin".into()),
        comment: (status == RequestStatus::Declined).then(|| "Already in the library".into()),
    }
}

fn issue(resolved: bool) -> IssueSummary {
    IssueSummary {
        id: 5,
        issue_type: IssueType::Audio,
        message: "Audio is out of sync after the first act.".into(),
        reported_by: "bob".into(),
        resolved_by: resolved.then(|| "admin".into()),
        comment: resolved.then(|| "Replaced with a different release".into()),
    }
}

fn health() -> HealthSummary {
    HealthSummary {
        source: "DownloadClientCheck".into(),
        level: HealthLevel::Error,
        message: "Unable to communicate with qBittorrent".into(),
        wiki_url: Some("https://reelix.example/docs/health#download-clients".into()),
    }
}

/// One sample event per event kind (the import sample appears twice: plain and upgrade).
pub fn all() -> Vec<NotificationEvent> {
    vec![
        NotificationEvent::test(),
        NotificationEvent::Grab {
            media: movie(),
            release: release(),
        },
        NotificationEvent::Import {
            media: movie(),
            release: Some(release()),
            file: file(),
            is_upgrade: false,
            replaced_files: vec![],
        },
        NotificationEvent::Import {
            media: series(),
            release: None,
            file: FileSummary {
                path: "/tv/Harbor Lights/Season 02/Harbor Lights - S02E05 - Low Tide.mkv".into(),
                quality: Some("WEBDL-2160p".into()),
                ..Default::default()
            },
            is_upgrade: true,
            replaced_files: vec![FileSummary {
                path: "/tv/Harbor Lights/Season 02/Harbor Lights - S02E05.mkv".into(),
                quality: Some("HDTV-720p".into()),
                ..Default::default()
            }],
        },
        NotificationEvent::Rename {
            media: series(),
            files: vec![RenamedFile {
                previous_path: "/tv/Harbor Lights/Season 2/harbor.lights.s02e05.mkv".into(),
                new_path: "/tv/Harbor Lights/Season 02/Harbor Lights - S02E05 - Low Tide.mkv".into(),
            }],
        },
        NotificationEvent::MediaAdded { media: movie() },
        NotificationEvent::MediaDeleted {
            media: movie(),
            files_deleted: true,
        },
        NotificationEvent::FileDeleted {
            media: movie(),
            file: file(),
            reason: DeleteReason::Upgrade,
        },
        NotificationEvent::HealthIssue(health()),
        NotificationEvent::HealthRestored(health()),
        NotificationEvent::ManualInteractionRequired {
            media: Some(movie()),
            release: release(),
            message: "Found matching movie via grab history, but release was matched to a different year"
                .into(),
        },
        NotificationEvent::RequestPending {
            media: series(),
            request: request(RequestStatus::Pending),
        },
        NotificationEvent::RequestApproved {
            media: series(),
            request: request(RequestStatus::Approved),
        },
        NotificationEvent::RequestDeclined {
            media: movie(),
            request: request(RequestStatus::Declined),
        },
        NotificationEvent::RequestAvailable {
            media: movie(),
            request: request(RequestStatus::Available),
        },
        NotificationEvent::IssueReported {
            media: movie(),
            issue: issue(false),
        },
        NotificationEvent::IssueResolved {
            media: movie(),
            issue: issue(true),
        },
        NotificationEvent::ApplicationUpdate(UpdateSummary {
            previous_version: "0.1.0".into(),
            new_version: "0.2.0".into(),
            release_notes_url: Some("https://reelix.example/releases/0.2.0".into()),
            changes: vec!["Faster parser".into(), "New notifiers".into()],
        }),
    ]
}
