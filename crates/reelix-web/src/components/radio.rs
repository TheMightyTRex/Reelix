//! Shared radio components: option lists (radio root folders, audiobook quality profiles,
//! tags), artwork and logo images, status / follow labels, episode numbers, the library
//! rescan and missing-search commands and indexer searches.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::content::ContentRootFolderDto;
use reelix_api_types::profiles::QualityProfile;
use reelix_api_types::providers::TagDto;
use reelix_api_types::radio::{
    RadioEpisodeDto, RadioEpisodeFileDto, RadioEpisodeStatus, RadioFollow, RadioSeriesDto, RadioSeriesKind,
    RadioShowDto,
};
use reelix_api_types::{CommandRequest, EnqueuedJob};

use crate::api::{self, ApiResult};
use crate::components::library_common::{LibIcon, bytes, toggle_id};
use crate::components::{Icon, Tone};
use crate::state::Toasts;

/// `entity.changed` entities that change what the radio pages show.
pub const RADIO_ENTITIES: &[&str] = &[
    "radio_show",
    "radio_episode",
    "radio_episode_file",
    "radio_station",
];

/// Radio root folders, audiobook quality profiles (radio uses the audiobook family) and tags.
#[derive(Clone, Copy)]
pub struct RadioOptions {
    pub roots: LocalResource<ApiResult<Vec<ContentRootFolderDto>>>,
    pub profiles: LocalResource<ApiResult<Vec<QualityProfile>>>,
    pub tags: LocalResource<ApiResult<Vec<TagDto>>>,
}

impl RadioOptions {
    pub fn load() -> Self {
        Self {
            roots: LocalResource::new(|| api::get::<Vec<ContentRootFolderDto>>("/radio/rootfolders")),
            profiles: LocalResource::new(|| {
                api::get::<Vec<QualityProfile>>("/qualityprofiles?kind=audiobook")
            }),
            tags: LocalResource::new(|| api::get::<Vec<TagDto>>("/tags")),
        }
    }

    pub fn root_list(&self) -> Vec<ContentRootFolderDto> {
        self.roots.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn profile_list(&self) -> Vec<QualityProfile> {
        self.profiles.get().and_then(Result::ok).unwrap_or_default()
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

    pub fn root_path(&self, id: Option<i64>) -> String {
        match id {
            None => "Radio library folder (settings)".into(),
            Some(id) => self
                .root_list()
                .into_iter()
                .find(|r| r.id == id)
                .map_or_else(|| format!("Root folder #{id}"), |r| r.path),
        }
    }

    pub fn profile_name(&self, id: Option<i64>) -> String {
        match id {
            None => "None".into(),
            Some(id) => self
                .profile_list()
                .into_iter()
                .find(|p| p.id == id)
                .map_or_else(|| format!("Profile #{id}"), |p| p.name),
        }
    }
}

/// `0` ↔ `None` for the id selects.
pub fn opt_id(v: i64) -> Option<i64> {
    (v != 0).then_some(v)
}

/// `<select>` of the radio root folders; `0` = the library folder from the radio settings.
#[component]
pub fn RadioRootSelect(opts: RadioOptions, value: RwSignal<i64>, #[prop(into)] id: String) -> impl IntoView {
    view! {
        <select id=id class="input" on:change=move |e| value.set(event_target_value(&e).parse().unwrap_or(0))>
            <option value="0" selected=move || value.get() == 0>"Library folder from the radio settings"</option>
            {move || opts.root_list().into_iter().map(|r| {
                let rid = r.id;
                let free = r.free_space.map(|f| format!(" ({} free)", bytes(f))).unwrap_or_default();
                view! { <option value=rid.to_string() selected=move || value.get() == rid>{format!("{}{free}", r.path)}</option> }
            }).collect_view()}
        </select>
    }
}

/// `<select>` of the audiobook quality profiles; `0` = none.
#[component]
pub fn RadioProfileSelect(
    opts: RadioOptions,
    value: RwSignal<i64>,
    #[prop(into)] id: String,
) -> impl IntoView {
    view! {
        <select id=id class="input" on:change=move |e| value.set(event_target_value(&e).parse().unwrap_or(0))>
            <option value="0" selected=move || value.get() == 0>"None"</option>
            {move || opts.profile_list().into_iter().map(|p| {
                let pid = p.id;
                view! { <option value=pid.to_string() selected=move || value.get() == pid>{p.name}</option> }
            }).collect_view()}
        </select>
    }
}

/// Toggleable tag chips.
#[component]
pub fn RadioTagPicker(opts: RadioOptions, value: RwSignal<Vec<i64>>) -> impl IntoView {
    view! {
        {move || {
            let tags = opts.tag_list();
            if tags.is_empty() {
                return view! { <p class="field-hint">"No tags yet — create them under Settings → Tags."</p> }.into_any();
            }
            view! {
                <div class="chip-list" role="group" aria-label="Tags">
                    {tags.into_iter().map(|t| {
                        let tid = t.id;
                        let on = move || value.with(|v| v.contains(&tid));
                        view! {
                            <button type="button" class="chip tag-chip" class:on=on aria-pressed=move || on().to_string()
                                on:click=move |_| toggle_id(value, tid)>
                                {t.label}
                            </button>
                        }
                    }).collect_view()}
                </div>
            }.into_any()
        }}
    }
}

pub const FOLLOW: [(RadioFollow, &str, &str); 3] = [
    (RadioFollow::All, "all", "Every episode, old and new"),
    (RadioFollow::FromNowOn, "from_now_on", "Episodes from now on"),
    (RadioFollow::None, "none", "None (keep the list only)"),
];

/// Short label of a follow choice.
pub fn follow_label(f: RadioFollow) -> &'static str {
    match f {
        RadioFollow::All => "All episodes",
        RadioFollow::FromNowOn => "From now on",
        RadioFollow::None => "Not followed",
    }
}

/// Status badge tone and label, with the same words and colours as books.
pub fn status_badge(s: RadioEpisodeStatus) -> (Tone, &'static str) {
    match s {
        RadioEpisodeStatus::InLibrary => (Tone::Success, "In library"),
        RadioEpisodeStatus::Wanted => (Tone::Danger, "Wanted"),
        RadioEpisodeStatus::NotReleased => (Tone::Info, "Not released"),
        RadioEpisodeStatus::NotRequested => (Tone::Neutral, "Not requested"),
    }
}

/// The series number episodes are filed under: specials are 0, an unnumbered series is 1.
fn filing_series(kind: RadioSeriesKind, number: Option<u32>) -> u32 {
    match kind {
        RadioSeriesKind::Specials => 0,
        RadioSeriesKind::Series => number.unwrap_or(1),
    }
}

/// `S20E03` (`S20E—` without an episode number).
pub fn episode_code(e: &RadioEpisodeDto) -> String {
    let s = filing_series(e.series_kind, e.series_number);
    match e.number {
        Some(n) => format!("S{s:02}E{n:02}"),
        None => format!("S{s:02}E—"),
    }
}

/// "Series 20", "Specials", or the series' own title.
pub fn series_label(s: &RadioSeriesDto) -> String {
    match s.kind {
        RadioSeriesKind::Specials => "Specials".into(),
        RadioSeriesKind::Series => {
            let n = s
                .number
                .map_or_else(|| "Series".into(), |n| format!("Series {n}"));
            match s.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                Some(t) if !t.eq_ignore_ascii_case(&n) => format!("{n}: {t}"),
                _ => n,
            }
        }
    }
}

/// `28:30` or `1:02:05` from seconds; `—` when unknown.
pub fn length(secs: Option<u32>) -> String {
    match secs {
        Some(s) if s >= 3600 => format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60),
        Some(s) => format!("{}:{:02}", s / 60, s % 60),
        None => "—".into(),
    }
}

/// One line describing a library file, for pickers.
pub fn file_label(f: &RadioEpisodeFileDto) -> String {
    let mut parts = Vec::new();
    if let Some(t) = f.item_title.as_deref().filter(|t| !t.is_empty()) {
        parts.push(t.to_string());
    }
    let code = match (f.season, f.episode) {
        (Some(s), Some(e)) => Some(format!("S{s:02}E{e:02}")),
        (None, Some(e)) => Some(format!("E{e:02}")),
        _ => None,
    };
    if let Some(c) = code {
        parts.push(c);
    }
    parts.push(
        f.title
            .clone()
            .or_else(|| f.path.clone())
            .unwrap_or_else(|| f.abs_episode_id.clone()),
    );
    if let Some(p) = f.published_at {
        parts.push(crate::format::date(p));
    }
    parts.join(" · ")
}

/// Served artwork of a show (`GET /radio/shows/{id}/artwork`), when one was downloaded.
pub fn artwork_url(show: &RadioShowDto) -> Option<String> {
    show.has_artwork.then(|| {
        let v = show.refreshed_at.map_or(0, |t| t.timestamp());
        api::api_url(&format!("/radio/shows/{}/artwork?v={v}", show.id))
    })
}

/// Artwork of a search / Discover hit when the page may load it: the image hosts the page's
/// Content-Security-Policy allows (`IMG_HOSTS` in `reelix-server/src/web.rs`: the BBC, Apple
/// and the Internet Archive). Other hosts get the placeholder (no image proxy).
pub fn hit_artwork(url: Option<&str>) -> Option<String> {
    let url = url?.trim();
    let host = url.strip_prefix("https://")?.split(['/', '?', '#']).next()?;
    let host = host.to_ascii_lowercase();
    let allowed = host == "ichef.bbci.co.uk"
        || host.ends_with(".mzstatic.com")
        || host == "archive.org"
        || host.ends_with(".archive.org");
    (allowed && !host.contains('@') && !host.contains(':')).then(|| url.to_owned())
}

/// Served logo of a station (`GET /radio/stations/{id}/logo`).
pub fn logo_url(station_id: i64) -> String {
    api::api_url(&format!("/radio/stations/{station_id}/logo"))
}

/// Square artwork letterboxed in the cover shape, with a radio icon when there is none.
#[component]
pub fn RadioArtwork(
    #[prop(into)] title: String,
    #[prop(into)] url: Option<String>,
    #[prop(optional, into)] class: Option<String>,
) -> impl IntoView {
    let failed = RwSignal::new(false);
    let class = format!("poster radio-art {}", class.unwrap_or_default());
    let alt = format!("Artwork for {title}");
    view! {
        <div class=class>
            {move || match url.clone().filter(|_| !failed.get()) {
                Some(src) => view! {
                    <img src=src alt=alt.clone() loading="lazy" decoding="async" on:error=move |_| failed.set(true) />
                }
                .into_any(),
                None => view! {
                    <div class="poster-fallback radio-art-fallback" role="img" aria-label=alt.clone()>
                        <Icon name="radio" />
                    </div>
                }
                .into_any(),
            }}
        </div>
    }
}

/// A station logo (small, decorative next to the station name).
#[component]
pub fn StationLogo(station_id: i64, #[prop(into)] name: String) -> impl IntoView {
    let failed = RwSignal::new(false);
    view! {
        <Show when=move || !failed.get()>
            <img class="station-logo" src=logo_url(station_id) alt=format!("{name} logo") loading="lazy"
                decoding="async" on:error=move |_| failed.set(true) />
        </Show>
    }
}

/// Bookmark button that monitors or unmonitors an episode (`label` names it for screen
/// readers).
#[component]
pub fn RadioMonitorToggle(
    monitored: bool,
    #[prop(into)] label: String,
    on_toggle: Callback<()>,
) -> impl IntoView {
    let text = format!("{} {label}", if monitored { "Unmonitor" } else { "Monitor" });
    view! {
        <button type="button" class="monitor-btn" class:off=!monitored aria-pressed=monitored.to_string()
            title=text.clone() aria-label=text on:click=move |e| { e.stop_propagation(); on_toggle.run(()); }>
            <LibIcon name=if monitored { "bookmark" } else { "bookmark-off" } />
        </button>
    }
}

/// A show tile on the shows grid.
#[component]
pub fn RadioShowCard(show: RadioShowDto, anchor: Option<String>) -> impl IntoView {
    let s = &show.statistics;
    let followed = show.follow != RadioFollow::None;
    let counts = format!("{} / {} episodes", s.in_library_count, s.episode_count);
    let wanted = s.wanted_count;
    view! {
        <div class="poster-card radio-card" id=anchor>
            <A href=format!("/radio/shows/{}", show.id) attr:class="poster-link">
                <div class="poster-wrap">
                    <RadioArtwork title=show.title.clone() url=artwork_url(&show) />
                    <span class="poster-monitor" class:off=!show.monitored title=if show.monitored { "Monitored" } else { "Unmonitored" }>
                        <span class="visually-hidden">{if show.monitored { "Monitored" } else { "Unmonitored" }}</span>
                    </span>
                </div>
                <div class="poster-meta">
                    <span class="poster-title" title=show.title.clone()>{show.title.clone()}</span>
                    {show.station_name.clone().map(|n| view! { <span class="poster-sub">{n}</span> })}
                    <span class="poster-sub">{counts}</span>
                    <span class="radio-card-badges">
                        <span class=format!("badge {}", if followed { Tone::Accent.class() } else { Tone::Neutral.class() })>
                            {follow_label(show.follow)}
                        </span>
                        {(!show.monitored).then(|| view! { <span class="badge tone-neutral">"Unmonitored"</span> })}
                        {(wanted > 0).then(|| view! { <span class="badge tone-danger">{format!("{wanted} wanted")}</span> })}
                    </span>
                </div>
            </A>
        </div>
    }
}

/// Queues a read of the Audiobookshelf podcast library (`POST /command RescanRadioLibrary`).
pub fn rescan_library() {
    let toasts = Toasts::expect();
    let req = CommandRequest {
        name: "RescanRadioLibrary".into(),
        show_ids: Vec::new(),
    };
    spawn_local(async move {
        match api::post::<_, EnqueuedJob>("/command", &req).await {
            Ok(job) if job.job_id.is_none() => toasts.info("A library rescan is already queued."),
            Ok(_) => toasts.success("Rescanning the podcast library in the background."),
            Err(e) => toasts.error(format!("Could not start the rescan: {e}")),
        }
    });
}

/// Queues an indexer search for every wanted radio episode (`POST /command RadioMissingSearch`).
pub fn search_all_missing() {
    let toasts = Toasts::expect();
    let req = CommandRequest {
        name: "RadioMissingSearch".into(),
        show_ids: Vec::new(),
    };
    spawn_local(async move {
        match api::post::<_, EnqueuedJob>("/command", &req).await {
            Ok(job) if job.job_id.is_none() => {
                toasts.info("A search for missing episodes is already queued.")
            }
            Ok(_) => toasts.success("Searching indexers for every wanted episode in the background."),
            Err(e) => toasts.error(format!("Could not start the search: {e}")),
        }
    });
}

/// Queues an indexer search: `/radio/shows/{id}/search` (the show's wanted episodes) or
/// `/radio/episodes/{id}/search` (one episode).
pub fn queue_search(path: String) {
    let toasts = Toasts::expect();
    spawn_local(async move {
        match api::post_empty::<EnqueuedJob>(&path).await {
            Ok(job) if job.job_id.is_none() => toasts.info("This search is already queued."),
            Ok(_) => toasts.success("Search queued; found releases are grabbed automatically."),
            Err(e) => toasts.error(format!("Could not start the search: {e}")),
        }
    });
}

/// The A–Z key of a sort title: an upper-case letter, or `#`.
pub fn letter_of(sort_title: &str) -> char {
    match sort_title.trim().chars().next() {
        Some(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase(),
        _ => '#',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(length(Some(1710)), "28:30");
        assert_eq!(length(Some(3725)), "1:02:05");
        assert_eq!(length(None), "—");
        assert_eq!(letter_of("archers, The"), 'A');
        assert_eq!(letter_of("1984"), '#');
        assert_eq!(filing_series(RadioSeriesKind::Specials, Some(4)), 0);
        assert_eq!(filing_series(RadioSeriesKind::Series, None), 1);
        assert_eq!(opt_id(0), None);
    }

    #[test]
    fn hit_artwork_follows_the_csp() {
        let ok = |u: &str| hit_artwork(Some(u)).is_some();
        assert!(ok("https://ichef.bbci.co.uk/images/ic/640x640/p0abc.jpg"));
        assert!(ok("https://is1-ssl.mzstatic.com/image/thumb/a.jpg/600x600bb.jpg"));
        assert!(ok("https://archive.org/services/img/x"));
        assert!(!ok("http://ichef.bbci.co.uk/x.jpg"));
        assert!(!ok("https://img.example.com/x.jpg"));
        assert!(!ok("https://evilmzstatic.com/x.jpg"));
        assert!(!ok("https://user@ichef.bbci.co.uk/x.jpg"));
        assert_eq!(hit_artwork(None), None);
    }
}
