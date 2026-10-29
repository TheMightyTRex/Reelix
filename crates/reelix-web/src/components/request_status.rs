//! Labels, tones and small views for request / issue / availability state.

use leptos::prelude::*;
use reelix_api_types::requests::{
    AvailabilityStatus, DiscoverItemDto, IssueStatus, IssueType, RequestBriefDto, RequestStatus, UserBriefDto,
};
use reelix_core::MediaKind;

use super::media_card::BadgeKind;
use super::{StatusBadge, Tone};
use crate::ws::LiveEvents;

pub fn request_status_label(s: RequestStatus) -> &'static str {
    match s {
        RequestStatus::Pending => "Pending",
        RequestStatus::Approved => "Approved",
        RequestStatus::Declined => "Declined",
        RequestStatus::Processing => "Processing",
        RequestStatus::PartiallyAvailable => "Partially available",
        RequestStatus::Available => "Available",
        RequestStatus::Failed => "Failed",
    }
}

pub fn request_status_tone(s: RequestStatus) -> Tone {
    match s {
        RequestStatus::Pending => Tone::Warning,
        RequestStatus::Approved => Tone::Info,
        RequestStatus::Declined => Tone::Neutral,
        RequestStatus::Processing => Tone::Accent,
        RequestStatus::PartiallyAvailable => Tone::Success,
        RequestStatus::Available => Tone::Success,
        RequestStatus::Failed => Tone::Danger,
    }
}

pub fn availability_label(a: AvailabilityStatus) -> &'static str {
    match a {
        AvailabilityStatus::Unknown => "Not in library",
        AvailabilityStatus::Monitored => "Monitored",
        AvailabilityStatus::Processing => "Processing",
        AvailabilityStatus::PartiallyAvailable => "Partially available",
        AvailabilityStatus::Available => "Available",
    }
}

pub fn availability_tone(a: AvailabilityStatus) -> Tone {
    match a {
        AvailabilityStatus::Unknown => Tone::Neutral,
        AvailabilityStatus::Monitored => Tone::Info,
        AvailabilityStatus::Processing => Tone::Accent,
        AvailabilityStatus::PartiallyAvailable => Tone::Success,
        AvailabilityStatus::Available => Tone::Success,
    }
}

pub fn issue_type_label(t: IssueType) -> &'static str {
    match t {
        IssueType::Video => "Video",
        IssueType::Audio => "Audio",
        IssueType::Subtitles => "Subtitles",
        IssueType::Other => "Other",
    }
}

pub const ISSUE_TYPES: [IssueType; 4] = [
    IssueType::Video,
    IssueType::Audio,
    IssueType::Subtitles,
    IssueType::Other,
];

pub fn issue_status_tone(s: IssueStatus) -> Tone {
    match s {
        IssueStatus::Open => Tone::Warning,
        IssueStatus::Resolved => Tone::Success,
    }
}

pub fn kind_label(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Movie => "Movie",
        MediaKind::Series => "Series",
    }
}

/// URL segment used by `/media/{seg}/{tmdbId}`.
pub fn kind_segment(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Movie => "movie",
        MediaKind::Series => "tv",
    }
}

/// `movie` / `series` as used by list filters and request options.
pub fn kind_param(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
    }
}

pub fn media_href(kind: MediaKind, tmdb_id: u32) -> String {
    format!("/media/{}/{tmdb_id}", kind_segment(kind))
}

/// The poster corner badge for a title: library state wins, then the request state.
pub fn badge_for(availability: AvailabilityStatus, request: Option<&RequestBriefDto>) -> Option<BadgeKind> {
    match availability {
        AvailabilityStatus::Available => return Some(BadgeKind::Available),
        AvailabilityStatus::PartiallyAvailable => return Some(BadgeKind::Partial),
        AvailabilityStatus::Processing => return Some(BadgeKind::Processing),
        _ => {}
    }
    match request.map(|r| r.status)? {
        RequestStatus::Pending | RequestStatus::Approved => Some(BadgeKind::Requested),
        RequestStatus::Processing => Some(BadgeKind::Processing),
        RequestStatus::PartiallyAvailable => Some(BadgeKind::Partial),
        RequestStatus::Available => Some(BadgeKind::Available),
        RequestStatus::Declined | RequestStatus::Failed => None,
    }
}

pub fn item_badge(item: &DiscoverItemDto) -> Option<BadgeKind> {
    badge_for(
        item.availability,
        item.request.as_ref().or(item.request_4k.as_ref()),
    )
}

#[component]
pub fn RequestStatusBadge(status: RequestStatus) -> impl IntoView {
    view! { <StatusBadge tone=request_status_tone(status) label=request_status_label(status) dot=true /> }
}

/// Avatar initial + display name.
#[component]
pub fn UserChip(user: UserBriefDto) -> impl IntoView {
    let initial = user
        .display_name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    let title = format!("@{}", user.username);
    view! {
        <span class="user-chip" title=title>
            {match user.avatar.filter(|a| a.starts_with("http")) {
                Some(src) => view! { <img class="avatar avatar-sm" src=src alt="" /> }.into_any(),
                None => view! { <span class="avatar avatar-sm" aria-hidden="true">{initial}</span> }.into_any(),
            }}
            <span>{user.display_name}</span>
        </span>
    }
}

/// Runs `refetch` whenever the server reports a change to one of `entities`.
pub fn refetch_on_change(entities: &'static [&'static str], refetch: impl Fn() + 'static) {
    let live = LiveEvents::expect();
    let last = StoredValue::new(live.events.with_untracked(|e| e.front().map_or(0, |e| e.seq)));
    Effect::new(move |_| {
        let Some(ev) = live.latest() else { return };
        if ev.seq <= last.get_value() {
            return;
        }
        last.set_value(ev.seq);
        if let reelix_api_types::DomainEvent::EntityChanged { entity, .. } = &ev.event
            && entities.contains(&entity.as_str())
        {
            refetch();
        }
    });
}

/// Percent-encodes a query-string component.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
