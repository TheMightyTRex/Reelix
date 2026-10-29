//! Shared music components: option lists (music quality profiles, metadata profiles, music
//! root folders, tags), album/artist state helpers, the search hand-off and the music dialogs.

mod modals;

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::content::{ContentRootFolderDto, ContentSearchKind};
use reelix_api_types::music::{
    AlbumDto, AlbumSearchRequest, AlbumType, ArtistDto, ArtistMonitor, MetadataProfileDto, NewAlbumMonitor,
    ReleaseStatus, SecondaryAlbumType, TrackDto,
};
use reelix_api_types::profiles::QualityProfile;
use reelix_api_types::providers::TagDto;

use crate::api::{self, ApiResult};
use crate::components::Tone;
use crate::components::library_common::bytes;
use crate::components::poster::MediaState;
use crate::state::Toasts;

pub use modals::{AddAlbumModal, AddArtistModal, ArtistEditModal, ChooseReleaseModal, MusicRenameModal};

/// Music quality profiles, metadata profiles, music root folders and tags — loaded once per page.
#[derive(Clone, Copy)]
pub struct MusicOptions {
    pub profiles: LocalResource<ApiResult<Vec<QualityProfile>>>,
    pub metadata_profiles: LocalResource<ApiResult<Vec<MetadataProfileDto>>>,
    pub roots: LocalResource<ApiResult<Vec<ContentRootFolderDto>>>,
    pub tags: LocalResource<ApiResult<Vec<TagDto>>>,
}

impl MusicOptions {
    pub fn load() -> Self {
        Self {
            profiles: LocalResource::new(|| api::get::<Vec<QualityProfile>>("/qualityprofiles?kind=music")),
            metadata_profiles: LocalResource::new(|| {
                api::get::<Vec<MetadataProfileDto>>("/metadataprofiles")
            }),
            roots: LocalResource::new(|| api::get::<Vec<ContentRootFolderDto>>("/musicrootfolders")),
            tags: LocalResource::new(|| api::get::<Vec<TagDto>>("/tags")),
        }
    }

    pub fn profile_list(&self) -> Vec<QualityProfile> {
        self.profiles.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn metadata_list(&self) -> Vec<MetadataProfileDto> {
        self.metadata_profiles
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
    }

    pub fn root_list(&self) -> Vec<ContentRootFolderDto> {
        self.roots.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn tag_list(&self) -> Vec<TagDto> {
        self.tags.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn tag_names(&self, ids: &[i64]) -> Vec<String> {
        let tags = self.tag_list();
        ids.iter()
            .map(|id| {
                tags.iter()
                    .find(|t| t.id == *id)
                    .map_or_else(|| format!("#{id}"), |t| t.label.clone())
            })
            .collect()
    }

    pub fn profile_name(&self, id: i64) -> String {
        self.profile_list()
            .into_iter()
            .find(|p| p.id == id)
            .map_or_else(|| format!("Profile #{id}"), |p| p.name)
    }

    pub fn metadata_name(&self, id: i64) -> String {
        self.metadata_list()
            .into_iter()
            .find(|p| p.id == id)
            .map_or_else(|| format!("Profile #{id}"), |p| p.name)
    }
}

/// `<select>` over `(id, label)` items; selects the first item while the value is `0`.
#[component]
pub fn IdSelect(
    #[prop(into)] items: Signal<Vec<(i64, String)>>,
    value: RwSignal<i64>,
    #[prop(into)] id: String,
) -> impl IntoView {
    Effect::new(move |_| {
        let list = items.get();
        if !list.iter().any(|(i, _)| *i == value.get_untracked())
            && let Some((first, _)) = list.first()
        {
            value.set(*first);
        }
    });
    view! {
        <select id=id class="input" on:change=move |e| value.set(event_target_value(&e).parse().unwrap_or(0))>
            {move || items.get().into_iter().map(|(i, l)| {
                view! { <option value=i.to_string() selected=move || value.get() == i>{l}</option> }
            }).collect_view()}
        </select>
    }
}

/// Quality, metadata and root-folder `(id, label)` lists for [`IdSelect`].
pub fn profile_items(opts: MusicOptions) -> Signal<Vec<(i64, String)>> {
    Signal::derive(move || opts.profile_list().into_iter().map(|p| (p.id, p.name)).collect())
}

pub fn metadata_items(opts: MusicOptions) -> Signal<Vec<(i64, String)>> {
    Signal::derive(move || opts.metadata_list().into_iter().map(|p| (p.id, p.name)).collect())
}

pub fn root_items(opts: MusicOptions) -> Signal<Vec<(i64, String)>> {
    Signal::derive(move || {
        opts.root_list()
            .into_iter()
            .map(|r| {
                let free = r
                    .free_space
                    .map(|f| format!(" ({} free)", bytes(f)))
                    .unwrap_or_default();
                (r.id, format!("{}{free}", r.path))
            })
            .collect()
    })
}

pub const ARTIST_MONITOR: [(ArtistMonitor, &str, &str); 7] = [
    (ArtistMonitor::All, "all", "All albums"),
    (ArtistMonitor::Future, "future", "Future albums"),
    (ArtistMonitor::Missing, "missing", "Missing albums"),
    (ArtistMonitor::Existing, "existing", "Existing albums"),
    (ArtistMonitor::Latest, "latest", "Latest album"),
    (ArtistMonitor::First, "first", "First album"),
    (ArtistMonitor::None, "none", "None"),
];

pub const NEW_ALBUM_MONITOR: [(NewAlbumMonitor, &str, &str); 3] = [
    (NewAlbumMonitor::All, "all", "Monitor all new albums"),
    (NewAlbumMonitor::New, "new", "Only albums released from now on"),
    (NewAlbumMonitor::None, "none", "Don't monitor new albums"),
];

pub const ALBUM_TYPES: [(AlbumType, &str); 5] = [
    (AlbumType::Album, "Album"),
    (AlbumType::Ep, "EP"),
    (AlbumType::Single, "Single"),
    (AlbumType::Broadcast, "Broadcast"),
    (AlbumType::Other, "Other"),
];

pub const SECONDARY_TYPES: [(SecondaryAlbumType, &str); 12] = [
    (SecondaryAlbumType::Compilation, "Compilation"),
    (SecondaryAlbumType::Soundtrack, "Soundtrack"),
    (SecondaryAlbumType::Spokenword, "Spoken word"),
    (SecondaryAlbumType::Interview, "Interview"),
    (SecondaryAlbumType::Audiobook, "Audiobook"),
    (SecondaryAlbumType::AudioDrama, "Audio drama"),
    (SecondaryAlbumType::Live, "Live"),
    (SecondaryAlbumType::Remix, "Remix"),
    (SecondaryAlbumType::DjMix, "DJ-mix"),
    (SecondaryAlbumType::Mixtape, "Mixtape/Street"),
    (SecondaryAlbumType::Demo, "Demo"),
    (SecondaryAlbumType::FieldRecording, "Field recording"),
];

pub const RELEASE_STATUSES: [(ReleaseStatus, &str); 4] = [
    (ReleaseStatus::Official, "Official"),
    (ReleaseStatus::Promotion, "Promotion"),
    (ReleaseStatus::Bootleg, "Bootleg"),
    (ReleaseStatus::PseudoRelease, "Pseudo-release"),
];

pub fn album_type_label(t: AlbumType) -> &'static str {
    ALBUM_TYPES
        .iter()
        .find(|(x, _)| *x == t)
        .map_or("Other", |(_, l)| l)
}

pub fn secondary_label(t: SecondaryAlbumType) -> &'static str {
    SECONDARY_TYPES
        .iter()
        .find(|(x, _)| *x == t)
        .map_or("", |(_, l)| l)
}

pub fn status_label(s: ReleaseStatus) -> &'static str {
    RELEASE_STATUSES
        .iter()
        .find(|(x, _)| *x == s)
        .map_or("", |(_, l)| l)
}

/// Section heading of an album on the artist page ("Album", "EP", "Live", …).
pub fn album_group(a: &AlbumDto) -> String {
    match a.secondary_types.first() {
        Some(s) => format!("{} ({})", secondary_label(*s), album_type_label(a.album_type)),
        None => album_type_label(a.album_type).to_string(),
    }
}

/// Poster/state of an album from its statistics.
pub fn album_state(a: &AlbumDto, today: chrono::NaiveDate) -> MediaState {
    let s = &a.statistics;
    if s.track_count > 0 && s.track_file_count >= s.track_count {
        MediaState::Downloaded
    } else if s.track_file_count > 0 {
        MediaState::Partial
    } else if !a.monitored {
        MediaState::Unmonitored
    } else if a.release_date.is_some_and(|d| d > today) {
        MediaState::Upcoming
    } else {
        MediaState::Missing
    }
}

/// Poster/state of an artist from its statistics.
pub fn artist_state(a: &ArtistDto) -> MediaState {
    let s = &a.statistics;
    if !a.monitored {
        MediaState::Unmonitored
    } else if s.track_count == 0 {
        if s.track_file_count > 0 {
            MediaState::Downloaded
        } else {
            MediaState::Unmonitored
        }
    } else if s.track_file_count == 0 {
        MediaState::Missing
    } else if s.track_file_count >= s.track_count {
        MediaState::Downloaded
    } else {
        MediaState::Partial
    }
}

/// File status of one track.
pub fn track_status(t: &TrackDto, album_monitored: bool) -> (Tone, String) {
    match &t.track_file {
        Some(f) if f.cutoff_met => (Tone::Success, f.quality_name.clone()),
        Some(f) => (Tone::Warning, f.quality_name.clone()),
        None if album_monitored => (Tone::Danger, "Missing".into()),
        None => (Tone::Neutral, "Unmonitored".into()),
    }
}

/// `3:07` from milliseconds.
pub fn duration(ms: Option<u64>) -> String {
    match ms {
        Some(ms) => {
            let s = ms / 1000;
            if s >= 3600 {
                format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
            } else {
                format!("{}:{:02}", s / 60, s % 60)
            }
        }
        None => "—".into(),
    }
}

/// Queues an album search (`POST /music/search`) and toasts the outcome.
pub fn start_album_search(kind: ContentSearchKind, ids: Vec<i64>, what: String) {
    let toasts = Toasts::expect();
    let req = AlbumSearchRequest { kind, ids };
    spawn_local(async move {
        match api::post::<_, EnqueuedJob>("/music/search", &req).await {
            Ok(job) if job.job_id.is_none() => toasts.info(format!("A search for {what} is already queued.")),
            Ok(_) => toasts.success(format!("Search queued for {what}.")),
            Err(e) => toasts.error(format!("Could not start the search: {e}")),
        }
    });
}

/// Monitors or unmonitors albums, then calls `done`.
pub fn set_albums_monitored(ids: Vec<i64>, on: bool, done: impl Fn() + 'static) {
    let toasts = Toasts::expect();
    let req = reelix_api_types::music::AlbumMonitorRequest {
        album_ids: ids,
        monitored: on,
    };
    spawn_local(async move {
        match api::put::<_, serde_json::Value>("/albums/monitor", &req).await {
            Ok(_) => done(),
            Err(e) => toasts.error(e.to_string()),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(Some(187_000)), "3:07");
        assert_eq!(duration(Some(3_725_000)), "1:02:05");
        assert_eq!(duration(None), "—");
        assert_eq!(album_type_label(AlbumType::Ep), "EP");
    }
}
