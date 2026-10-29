//! Library form helpers: option lists (profiles, root folders, tags), pickers, the search
//! hand-off, delete / rename dialogs and a few HTTP helpers the generic client lacks.

use gloo_net::http::Request;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use reelix_api_types::library::{
    CalendarKind, LibrarySearchKind, MediaKind, RenamePreviewDto, RenameRequest, RootFolderDto, SearchRequest,
};
use reelix_api_types::profiles::QualityProfile;
use reelix_api_types::providers::TagDto;
use reelix_api_types::{ApiErrorBody, CSRF_HEADER, EnqueuedJob, Permissions};
use serde::Serialize;

use super::{EmptyState, Icon, Modal, Skeleton, Spinner};
use crate::api::{self, ApiError, ApiResult};
use crate::state::{AuthState, Toasts};

/// Sends users without `MANAGE_LIBRARY` to Discover.
pub fn require_library() {
    require_any(Permissions::MANAGE_LIBRARY);
}

/// Sends users holding none of `any` to Discover.
pub fn require_any(any: Permissions) {
    let auth = AuthState::expect();
    let navigate = use_navigate();
    Effect::new(move |_| {
        if auth.is_logged_in() && !auth.has_any(any) {
            navigate("/discover", Default::default());
        }
    });
}

/// Percent-encodes a query-string value.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Builds `path?k=v&…`, skipping empty values.
pub fn with_query(path: &str, params: &[(&str, String)]) -> String {
    let qs: Vec<String> = params
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    if qs.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", qs.join("&"))
    }
}

/// `DELETE {path}` with a JSON body (bulk deletes).
pub async fn delete_json<B: Serialize>(path: &str, body: &B) -> ApiResult<()> {
    let req = Request::delete(&api::api_url(path))
        .header(CSRF_HEADER, "1")
        .header("accept", "application/json")
        .json(body)
        .map_err(|e| ApiError {
            status: 0,
            code: "decode".into(),
            message: e.to_string(),
        })?;
    let resp = req.send().await.map_err(|e| ApiError {
        status: 0,
        code: "network".into(),
        message: format!("Could not reach the Reelix server ({e})."),
    })?;
    if resp.ok() {
        return Ok(());
    }
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    Err(match serde_json::from_str::<ApiErrorBody>(&text) {
        Ok(b) => ApiError {
            status,
            code: b.error,
            message: b.message,
        },
        Err(_) => ApiError {
            status,
            code: format!("http_{status}"),
            message: format!("Request failed ({status})."),
        },
    })
}

/// Queues a library search (handled by the search pipeline) and toasts the outcome.
pub fn start_search(
    kind: LibrarySearchKind,
    ids: Vec<i64>,
    season: Option<u32>,
    media: Option<CalendarKind>,
    what: String,
) {
    let toasts = Toasts::expect();
    let req = SearchRequest {
        kind,
        ids,
        season,
        media,
    };
    spawn_local(async move {
        match api::post::<_, EnqueuedJob>("/search", &req).await {
            Ok(job) if job.job_id.is_none() => toasts.info(format!("A search for {what} is already queued.")),
            Ok(_) => toasts.success(format!("Search queued for {what}.")),
            Err(e) => toasts.error(format!("Could not start the search: {e}")),
        }
    });
}

/// Fires a POST whose response body isn't needed (refresh / rescan) and toasts.
pub fn fire(path: String, ok: String) {
    let toasts = Toasts::expect();
    spawn_local(async move {
        match api::post_empty::<serde_json::Value>(&path).await {
            Ok(_) => toasts.success(ok),
            Err(e) => toasts.error(e.to_string()),
        }
    });
}

/// Quality profiles, root folders and tags — loaded once per page.
#[derive(Clone, Copy)]
pub struct LibOptions {
    pub profiles: LocalResource<ApiResult<Vec<QualityProfile>>>,
    pub roots: LocalResource<ApiResult<Vec<RootFolderDto>>>,
    pub tags: LocalResource<ApiResult<Vec<TagDto>>>,
}

impl LibOptions {
    pub fn load() -> Self {
        Self {
            profiles: LocalResource::new(|| api::get::<Vec<QualityProfile>>("/qualityprofiles")),
            roots: LocalResource::new(|| api::get::<Vec<RootFolderDto>>("/rootfolders")),
            tags: LocalResource::new(|| api::get::<Vec<TagDto>>("/tags")),
        }
    }

    pub fn profile_list(&self) -> Vec<QualityProfile> {
        self.profiles.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn root_list(&self, kind: MediaKind) -> Vec<RootFolderDto> {
        self.roots
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.media_kind == kind)
            .collect()
    }

    pub fn tag_list(&self) -> Vec<TagDto> {
        self.tags.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn profile_name(&self, id: i64) -> String {
        self.profiles
            .get()
            .and_then(Result::ok)
            .and_then(|l| l.into_iter().find(|p| p.id == id).map(|p| p.name))
            .unwrap_or_else(|| format!("Profile #{id}"))
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
}

/// `<select>` of quality profiles. `0` means "unset" and is shown as a prompt when `allow_none`.
#[component]
pub fn ProfileSelect(
    opts: LibOptions,
    value: RwSignal<i64>,
    #[prop(into)] id: String,
    #[prop(optional, into)] none_label: Option<String>,
) -> impl IntoView {
    // Default to the first profile when nothing is chosen and "none" isn't allowed.
    let allow_none = none_label.is_some();
    Effect::new(move |_| {
        let list = opts.profile_list();
        if !allow_none
            && value.get_untracked() == 0
            && let Some(first) = list.first()
        {
            value.set(first.id);
        }
    });
    view! {
        <select id=id class="input" on:change=move |e| value.set(event_target_value(&e).parse().unwrap_or(0))>
            {none_label.map(|l| view! { <option value="0" selected=move || value.get() == 0>{l}</option> })}
            {move || opts.profile_list().into_iter().map(|p| {
                let pid = p.id;
                view! { <option value=pid.to_string() selected=move || value.get() == pid>{p.name}</option> }
            }).collect_view()}
        </select>
    }
}

/// `<select>` of root folders for one media kind, showing free space.
#[component]
pub fn RootFolderSelect(
    opts: LibOptions,
    kind: MediaKind,
    value: RwSignal<i64>,
    #[prop(into)] id: String,
    #[prop(optional, into)] none_label: Option<String>,
) -> impl IntoView {
    let allow_none = none_label.is_some();
    Effect::new(move |_| {
        let list = opts.root_list(kind);
        if !allow_none
            && value.get_untracked() == 0
            && let Some(first) = list.first()
        {
            value.set(first.id);
        }
    });
    view! {
        <select id=id class="input" on:change=move |e| value.set(event_target_value(&e).parse().unwrap_or(0))>
            {none_label.map(|l| view! { <option value="0" selected=move || value.get() == 0>{l}</option> })}
            {move || opts.root_list(kind).into_iter().map(|r| {
                let rid = r.id;
                let free = r.free_space.map(|f| format!(" ({} free)", super::library_common::bytes(f))).unwrap_or_default();
                view! { <option value=rid.to_string() selected=move || value.get() == rid>{format!("{}{free}", r.path)}</option> }
            }).collect_view()}
        </select>
    }
}

/// Toggleable tag chips (tags are managed under Settings → Tags).
#[component]
pub fn TagPicker(opts: LibOptions, value: RwSignal<Vec<i64>>) -> impl IntoView {
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
                                on:click=move |_| super::library_common::toggle_id(value, tid)>
                                {t.label}
                            </button>
                        }
                    }).collect_view()}
                </div>
            }.into_any()
        }}
    }
}

/// Delete confirmation with an "also delete files" option.
#[component]
pub fn DeleteMediaDialog(
    open: RwSignal<bool>,
    #[prop(into)] title: String,
    #[prop(into)] message: Signal<String>,
    on_confirm: Callback<bool>,
) -> impl IntoView {
    let delete_files = RwSignal::new(false);
    view! {
        <Modal open=open title=title>
            <p class="modal-text">{move || message.get()}</p>
            <label class="checkbox delete-files">
                <input type="checkbox" prop:checked=move || delete_files.get()
                    on:change=move |e| delete_files.set(event_target_checked(&e)) />
                "Also delete the files and folder (to the recycle bin when one is configured)"
            </label>
            <div class="modal-footer">
                <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                <button type="button" class="btn btn-danger" on:click=move |_| {
                    open.set(false);
                    on_confirm.run(delete_files.get_untracked());
                    delete_files.set(false);
                }>
                    <Icon name="trash" />
                    "Delete"
                </button>
            </div>
        </Modal>
    }
}

/// Lists files whose names differ from the naming templates and renames them on confirm.
#[component]
pub fn RenameModal(open: RwSignal<bool>, kind: MediaKind, media_id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let query = match kind {
        MediaKind::Movie => format!("/rename/preview?movie_id={media_id}"),
        MediaKind::Series => format!("/rename/preview?series_id={media_id}"),
    };
    let preview = LocalResource::new(move || {
        let q = query.clone();
        let o = open.get();
        async move {
            if o {
                Some(api::get::<Vec<RenamePreviewDto>>(&q).await)
            } else {
                None
            }
        }
    });
    let busy = RwSignal::new(false);
    let rename = move |_| {
        busy.set(true);
        let req = RenameRequest {
            kind,
            ids: vec![media_id],
            file_ids: None,
        };
        spawn_local(async move {
            match api::post::<_, serde_json::Value>("/rename", &req).await {
                Ok(_) => {
                    toasts.success("Rename queued.");
                    open.set(false);
                }
                Err(e) => toasts.error(format!("Could not rename: {e}")),
            }
            busy.set(false);
        });
    };
    view! {
        <Modal open=open title="Rename files">
            <div class="modal-wide">
                {move || match preview.get().flatten() {
                    None => view! { <Skeleton lines=3 /> }.into_any(),
                    Some(Err(e)) => view! { <super::ErrorBox error=e /> }.into_any(),
                    Some(Ok(list)) if list.is_empty() => view! {
                        <EmptyState icon="check" title="Nothing to rename" description="Every file already matches your naming settings." compact=true />
                    }.into_any(),
                    Some(Ok(list)) => {
                        let n = list.len();
                        view! {
                            <p class="modal-text">{format!("{n} file(s) will be renamed:")}</p>
                            <div class="rename-list">
                                {list.into_iter().map(|r| view! {
                                    <div class="rename-item">
                                        <div class="from">{format!("− {}", r.existing_path)}</div>
                                        <div class="to">{format!("+ {}", r.new_path)}</div>
                                    </div>
                                }).collect_view()}
                            </div>
                            <div class="modal-footer">
                                <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                                <button type="button" class="btn btn-primary" disabled=move || busy.get() on:click=rename>
                                    {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="check" /> }.into_any() }}
                                    "Rename"
                                </button>
                            </div>
                        }.into_any()
                    }
                }}
            </div>
        </Modal>
    }
}

// ---------------- enum labels / option lists ----------------

use reelix_api_types::library::{
    MinimumAvailability, MovieMonitor, MovieStatus, SeriesMonitor, SeriesStatus, SeriesType,
};

pub const MIN_AVAILABILITY: [(MinimumAvailability, &str, &str); 3] = [
    (MinimumAvailability::Announced, "announced", "Announced"),
    (MinimumAvailability::InCinemas, "in_cinemas", "In cinemas"),
    (
        MinimumAvailability::Released,
        "released",
        "Released (digital / physical)",
    ),
];

pub const MOVIE_MONITOR: [(MovieMonitor, &str, &str); 3] = [
    (MovieMonitor::MovieOnly, "movie_only", "Movie only"),
    (
        MovieMonitor::MovieAndCollection,
        "movie_and_collection",
        "Movie and collection",
    ),
    (MovieMonitor::None, "none", "None"),
];

pub const SERIES_MONITOR: [(SeriesMonitor, &str, &str); 8] = [
    (SeriesMonitor::All, "all", "All episodes"),
    (SeriesMonitor::Future, "future", "Future episodes"),
    (SeriesMonitor::Missing, "missing", "Missing episodes"),
    (SeriesMonitor::Existing, "existing", "Existing episodes"),
    (SeriesMonitor::FirstSeason, "first_season", "First season"),
    (SeriesMonitor::LatestSeason, "latest_season", "Latest season"),
    (SeriesMonitor::Pilot, "pilot", "Pilot episode"),
    (SeriesMonitor::None, "none", "None"),
];

pub const SERIES_TYPES: [(SeriesType, &str, &str); 3] = [
    (SeriesType::Standard, "standard", "Standard (S01E05)"),
    (SeriesType::Daily, "daily", "Daily (2024-05-18)"),
    (SeriesType::Anime, "anime", "Anime (absolute numbers)"),
];

/// Finds the value for an option key in one of the tables above.
pub fn opt_from<T: Copy>(table: &[(T, &str, &str)], key: &str) -> Option<T> {
    table.iter().find(|(_, k, _)| *k == key).map(|(v, _, _)| *v)
}

/// Option key for a value.
pub fn opt_key<T: Copy + PartialEq>(table: &[(T, &'static str, &'static str)], v: T) -> &'static str {
    table.iter().find(|(x, _, _)| *x == v).map_or("", |(_, k, _)| k)
}

/// Option label for a value.
pub fn opt_label<T: Copy + PartialEq>(table: &[(T, &'static str, &'static str)], v: T) -> &'static str {
    table.iter().find(|(x, _, _)| *x == v).map_or("", |(_, _, l)| l)
}

/// `<select>` bound to an enum signal using one of the option tables.
#[component]
pub fn EnumSelect<T>(
    table: &'static [(T, &'static str, &'static str)],
    value: RwSignal<T>,
    #[prop(into)] id: String,
) -> impl IntoView
where
    T: Copy + PartialEq + Send + Sync + 'static,
{
    view! {
        <select id=id class="input" on:change=move |e| {
            if let Some(v) = opt_from(table, &event_target_value(&e)) {
                value.set(v);
            }
        }>
            {table.iter().map(|(v, k, l)| {
                let v = *v;
                view! { <option value=*k selected=move || value.get() == v>{*l}</option> }
            }).collect_view()}
        </select>
    }
}

pub fn movie_status_label(s: MovieStatus) -> &'static str {
    match s {
        MovieStatus::Tba => "TBA",
        MovieStatus::Announced => "Announced",
        MovieStatus::InCinemas => "In cinemas",
        MovieStatus::Released => "Released",
        MovieStatus::Deleted => "Removed from TMDB",
    }
}

pub fn series_status_label(s: SeriesStatus) -> &'static str {
    match s {
        SeriesStatus::Continuing => "Continuing",
        SeriesStatus::Ended => "Ended",
        SeriesStatus::Upcoming => "Upcoming",
        SeriesStatus::Deleted => "Removed from TMDB",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_query_strings() {
        assert_eq!(enc("a b&c/é"), "a%20b%26c%2F%C3%A9");
        assert_eq!(
            with_query(
                "/movies",
                &[
                    ("q", "star wars".into()),
                    ("tag", String::new()),
                    ("dir", "asc".into())
                ]
            ),
            "/movies?q=star%20wars&dir=asc"
        );
        assert_eq!(with_query("/x", &[("a", String::new())]), "/x");
    }

    #[test]
    fn option_tables_round_trip() {
        for (v, k, _) in SERIES_MONITOR {
            assert_eq!(opt_from(&SERIES_MONITOR, k), Some(v));
            assert_eq!(opt_key(&SERIES_MONITOR, v), k);
        }
        assert_eq!(opt_from(&MIN_AVAILABILITY, "nope"), None);
    }
}
