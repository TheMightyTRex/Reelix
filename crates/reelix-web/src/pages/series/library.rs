//! `/series` — the series library with episode progress, filters, sort and bulk edits.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::library::{
    BulkDeleteRequest, CalendarKind, LibrarySearchKind, MediaKind, Paged, SeasonPassRequest, SeriesBulkEdit,
    SeriesDto, SeriesEdit, SeriesMonitor, SeriesStatus, SeriesType,
};

use crate::api;
use crate::components::library_bulk::BulkEditModal;
use crate::components::library_common::{
    BulkBar, LibIcon, SortHeader, ViewMode, ViewSwitcher, bytes, on_entity_changed, pref_get, pref_set,
    toggle_id,
};
use crate::components::library_forms::{
    DeleteMediaDialog, EnumSelect, LibOptions, ProfileSelect, SERIES_MONITOR, SERIES_TYPES, delete_json,
    require_library, series_status_label, start_search, with_query,
};
use crate::components::poster::{
    EpisodeProgress, MediaState, POSTER_MD, POSTER_SM, Poster, PosterCard, tmdb_image,
};
use crate::components::{EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton, StatusBadge};
use crate::state::Toasts;

const PAGE_CHUNK: usize = 120;

pub fn series_state(s: &SeriesDto) -> MediaState {
    let st = &s.statistics;
    if !s.monitored {
        MediaState::Unmonitored
    } else if st.episode_count == 0 {
        MediaState::Upcoming
    } else if st.episode_file_count >= st.episode_count {
        MediaState::Downloaded
    } else if st.episode_file_count == 0 {
        MediaState::Missing
    } else {
        MediaState::Partial
    }
}

const SORTS: [(&str, &str); 9] = [
    ("title", "Title"),
    ("year", "Year"),
    ("added", "Date added"),
    ("next_airing", "Next airing"),
    ("network", "Network"),
    ("progress", "Episode progress"),
    ("size", "Size on disk"),
    ("rating", "Rating"),
    ("status", "Status"),
];

const STATUSES: [(SeriesStatus, &str); 4] = [
    (SeriesStatus::Continuing, "continuing"),
    (SeriesStatus::Ended, "ended"),
    (SeriesStatus::Upcoming, "upcoming"),
    (SeriesStatus::Deleted, "deleted"),
];

#[component]
pub fn SeriesPage() -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let opts = LibOptions::load();

    let mode = RwSignal::new(ViewMode::load("series"));
    let q = RwSignal::new(String::new());
    let quick = RwSignal::new(pref_get("series.filter").unwrap_or_else(|| "all".into()));
    let sort = RwSignal::new(pref_get("series.sort").unwrap_or_else(|| "title".into()));
    let desc = RwSignal::new(pref_get("series.desc").as_deref() == Some("1"));
    let show_filters = RwSignal::new(false);
    let f_profile = RwSignal::new(0i64);
    let f_tag = RwSignal::new(String::new());
    let f_genre = RwSignal::new(String::new());
    let f_network = RwSignal::new(String::new());
    let f_year = RwSignal::new(String::new());
    let f_status = RwSignal::new(String::new());
    let f_type = RwSignal::new(String::new());
    let shown = RwSignal::new(PAGE_CHUNK);

    Effect::new(move |_| {
        pref_set("series.filter", &quick.get());
        pref_set("series.sort", &sort.get());
        pref_set("series.desc", if desc.get() { "1" } else { "0" });
    });

    let series = LocalResource::new(move || {
        let mut p: Vec<(&str, String)> = vec![
            ("sort", sort.get()),
            ("dir", if desc.get() { "desc" } else { "asc" }.into()),
            ("q", q.get().trim().to_string()),
        ];
        match quick.get().as_str() {
            "monitored" => p.push(("monitored", "true".into())),
            "unmonitored" => p.push(("monitored", "false".into())),
            "missing" => p.push(("missing", "true".into())),
            "continuing" => p.push(("status", "continuing".into())),
            "ended" => p.push(("status", "ended".into())),
            _ => {}
        }
        let prof = f_profile.get();
        if prof != 0 {
            p.push(("quality_profile_id", prof.to_string()));
        }
        p.push(("tag", f_tag.get()));
        p.push(("genre", f_genre.get().trim().to_string()));
        p.push(("network", f_network.get().trim().to_string()));
        p.push(("year", f_year.get().trim().to_string()));
        if !matches!(quick.get().as_str(), "continuing" | "ended") {
            p.push(("status", f_status.get()));
        }
        p.push(("series_type", f_type.get()));
        let url = with_query("/series", &p);
        async move { api::get::<Paged<SeriesDto>>(&url).await }
    });
    on_entity_changed(
        &["series", "season", "episode", "episode_file", "episodefile"],
        true,
        move || series.refetch(),
    );

    let selecting = RwSignal::new(false);
    let selected = RwSignal::new(Vec::<i64>::new());
    let all_ids = move || {
        series
            .get()
            .and_then(Result::ok)
            .map(|p| p.items.iter().map(|s| s.id).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let edit_open = RwSignal::new(false);
    let delete_open = RwSignal::new(false);
    let pass_open = RwSignal::new(false);
    let pass_monitor = RwSignal::new(SeriesMonitor::All);

    let set_monitored = move |on: bool| {
        let ids = selected.get_untracked();
        let n = ids.len();
        spawn_local(async move {
            let body = SeriesBulkEdit {
                ids,
                edit: SeriesEdit {
                    monitored: Some(on),
                    ..Default::default()
                },
            };
            match api::put::<_, serde_json::Value>("/series/bulk", &body).await {
                Ok(_) => {
                    toasts.success(format!(
                        "{} {n} series.",
                        if on { "Monitored" } else { "Unmonitored" }
                    ));
                    series.refetch();
                }
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };
    let season_pass = move |_| {
        let req = SeasonPassRequest {
            series_ids: selected.get_untracked(),
            monitor: pass_monitor.get_untracked(),
            monitored: None,
        };
        spawn_local(async move {
            match api::post::<_, serde_json::Value>("/seasonpass", &req).await {
                Ok(_) => {
                    toasts.success("Season pass applied.");
                    pass_open.set(false);
                    series.refetch();
                }
                Err(e) => toasts.error(format!("Could not apply: {e}")),
            }
        });
    };
    let bulk_delete = Callback::new(move |delete_files: bool| {
        let ids = selected.get_untracked();
        let n = ids.len();
        spawn_local(async move {
            match delete_json("/series/bulk", &BulkDeleteRequest { ids, delete_files }).await {
                Ok(()) => {
                    toasts.success(format!("Deleted {n} series."));
                    selected.set(Vec::new());
                    series.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });
    let update_all = move |_| {
        spawn_local(async move {
            match api::post_empty::<EnqueuedJob>("/system/tasks/refresh_series/run").await {
                Ok(_) => toasts.success("Refreshing every series in the background."),
                Err(e) => toasts.error(format!("Could not start the refresh: {e}")),
            }
        });
    };

    let count_label = move || {
        series
            .get()
            .and_then(Result::ok)
            .map(|p| format!("{} series", p.total))
            .unwrap_or_default()
    };

    let content = move || {
        let page = match series.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| series.refetch()) /> }
                    .into_any();
            }
            Some(Ok(p)) => p,
        };
        if page.items.is_empty() {
            let filtered = quick.get_untracked() != "all" || !q.get_untracked().is_empty();
            return if filtered {
                view! { <div class="card"><EmptyState icon="search" title="No series match" description="Try clearing the search or filters." /></div> }.into_any()
            } else {
                view! {
                    <div class="card">
                        <EmptyState icon="series" title="No series yet" description="Add shows from TMDB or import folders you already have.">
                            <A href="/series/add" attr:class="btn btn-primary"><Icon name="plus" />"Add series"</A>
                            <A href="/series/import" attr:class="btn btn-secondary"><Icon name="folder" />"Import library"</A>
                        </EmptyState>
                    </div>
                }
                .into_any()
            };
        }
        let total = page.items.len();
        let items: Vec<SeriesDto> = page.items.into_iter().take(shown.get()).collect();
        let more = (total > items.len()).then(|| {
            view! {
                <div class="empty-actions">
                    <button type="button" class="btn btn-secondary" on:click=move |_| shown.update(|s| *s += PAGE_CHUNK)>
                        {format!("Show more ({} left)", total - shown.get_untracked().min(total))}
                    </button>
                </div>
            }
        });
        let body = match mode.get() {
            ViewMode::Posters => view! {
                <div class="poster-grid">
                    {items.into_iter().map(|s| {
                        let id = s.id;
                        let st = &s.statistics;
                        let sub = [s.year.map(|y| y.to_string()), s.network.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                        view! {
                            <PosterCard
                                href=format!("/series/{id}")
                                title=s.title.clone()
                                subtitle=sub
                                poster=tmdb_image(s.images.poster.as_deref(), POSTER_MD)
                                state=series_state(&s)
                                monitored=s.monitored
                                progress=(st.episode_file_count, st.episode_count)
                                selected=Signal::derive(move || selected.with(|x| x.contains(&id)))
                                on_toggle=Callback::new(move |()| toggle_id(selected, id))
                                select_mode=selecting
                            />
                        }
                    }).collect_view()}
                </div>
            }
            .into_any(),
            ViewMode::Overview => view! {
                <div class="overview-list">
                    {items.into_iter().map(|s| {
                        let id = s.id;
                        let state = series_state(&s);
                        let navigate = navigate.clone();
                        let nav2 = navigate.clone();
                        let st = s.statistics.clone();
                        view! {
                            <div class="overview-item lib-row" class:selected=move || selected.with(|x| x.contains(&id))
                                role="link" tabindex="0"
                                on:click=move |_| if selecting.get_untracked() { toggle_id(selected, id) } else { navigate(&format!("/series/{id}"), Default::default()) }
                                on:keydown=move |e: leptos::ev::KeyboardEvent| if e.key() == "Enter" {
                                    if selecting.get_untracked() { toggle_id(selected, id) } else { nav2(&format!("/series/{id}"), Default::default()) }
                                }>
                                <Poster title=s.title.clone() url=tmdb_image(s.images.poster.as_deref(), POSTER_SM) />
                                <div class="overview-body">
                                    <span class="overview-title">{s.title.clone()}{s.year.map(|y| format!(" ({y})"))}</span>
                                    <div class="meta-line">
                                        <StatusBadge tone=state.tone() label=state.label() dot=true />
                                        <span>{series_status_label(s.status)}</span>
                                        {s.network.clone().map(|n| view! { <span>{n}</span> })}
                                        <span>{format!("{} season(s)", st.season_count)}</span>
                                        <span>{opts.profile_name(s.quality_profile_id)}</span>
                                        {(st.size_on_disk > 0).then(|| view! { <span>{bytes(st.size_on_disk)}</span> })}
                                        {s.next_airing.map(|d| view! { <span>{format!("Next: {}", d.format("%Y-%m-%d"))}</span> })}
                                    </div>
                                    <EpisodeProgress have=st.episode_file_count total=st.episode_count />
                                    {s.overview.clone().map(|o| view! { <p class="overview-text">{o}</p> })}
                                </div>
                            </div>
                        }
                    }).collect_view()}
                </div>
            }
            .into_any(),
            ViewMode::Table => view! {
                <div class="card">
                    <div class="card-body">
                        <div class="table-wrap">
                            <table class="table">
                                <caption class="visually-hidden">"Series"</caption>
                                <thead>
                                    <tr>
                                        {move || selecting.get().then(|| view! { <th scope="col" class="col-check"><span class="visually-hidden">"Selected"</span></th> })}
                                        <th scope="col"><span class="visually-hidden">"Status"</span></th>
                                        <SortHeader label="Title" key="title" sort=sort desc=desc />
                                        <SortHeader label="Network" key="network" sort=sort desc=desc />
                                        <th scope="col">"Seasons"</th>
                                        <th scope="col">"Profile"</th>
                                        <SortHeader label="Next airing" key="next_airing" sort=sort desc=desc />
                                        <SortHeader label="Episodes" key="progress" sort=sort desc=desc />
                                        <SortHeader label="Size" key="size" sort=sort desc=desc />
                                    </tr>
                                </thead>
                                <tbody>
                                    {items.into_iter().map(|s| {
                                        let id = s.id;
                                        let state = series_state(&s);
                                        let st = s.statistics.clone();
                                        let is_sel = move || selected.with(|x| x.contains(&id));
                                        view! {
                                            <tr class:is-selected=is_sel>
                                                {move || selecting.get().then(|| view! {
                                                    <td class="col-check">
                                                        <input type="checkbox" aria-label=format!("Select series {id}") prop:checked=is_sel
                                                            on:change=move |_| toggle_id(selected, id) />
                                                    </td>
                                                })}
                                                <td><span class=format!("status-dot {}", state.class()) title=state.label()></span></td>
                                                <td class="cell-strong">
                                                    <A href=format!("/series/{id}")>{s.title.clone()}</A>
                                                    <span class="cell-sub">{series_status_label(s.status)}</span>
                                                </td>
                                                <td>{s.network.clone().unwrap_or_else(|| "—".into())}</td>
                                                <td>{st.season_count}</td>
                                                <td>{opts.profile_name(s.quality_profile_id)}</td>
                                                <td>{s.next_airing.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "—".into())}</td>
                                                <td><EpisodeProgress have=st.episode_file_count total=st.episode_count /></td>
                                                <td>{if st.size_on_disk > 0 { bytes(st.size_on_disk) } else { "—".into() }}</td>
                                            </tr>
                                        }
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    </div>
                </div>
            }
            .into_any(),
        };
        view! { {body} {more} }.into_any()
    };

    view! {
        <PageHeader title="Series">
            <button type="button" class="btn btn-secondary" on:click=update_all title="Refresh metadata for every series">
                <Icon name="refresh" />
                <span>"Update all"</span>
            </button>
            <button type="button" class="btn btn-secondary" title="Search for every missing monitored episode"
                on:click=move |_| start_search(LibrarySearchKind::Missing, Vec::new(), None, Some(CalendarKind::Episode), "all missing episodes".into())>
                <Icon name="search" />
                <span>"Search missing"</span>
            </button>
            <A href="/series/add" attr:class="btn btn-primary"><Icon name="plus" />"Add series"</A>
        </PageHeader>

        <div class="lib-toolbar">
            <input class="input grow" type="search" placeholder="Filter by title…" aria-label="Filter by title"
                prop:value=move || q.get() on:input=move |e| { q.set(event_target_value(&e)); shown.set(PAGE_CHUNK); } />
            <select class="input" aria-label="Show" on:change=move |e| quick.set(event_target_value(&e))>
                {[("all", "All series"), ("monitored", "Monitored"), ("unmonitored", "Unmonitored"), ("missing", "Missing episodes"),
                  ("continuing", "Continuing"), ("ended", "Ended")]
                    .into_iter()
                    .map(|(v, l)| view! { <option value=v selected=move || quick.get() == v>{l}</option> })
                    .collect_view()}
            </select>
            <select class="input" aria-label="Sort by" on:change=move |e| sort.set(event_target_value(&e))>
                {SORTS.into_iter().map(|(v, l)| view! { <option value=v selected=move || sort.get() == v>{l}</option> }).collect_view()}
            </select>
            <button type="button" class="icon-btn" title=move || if desc.get() { "Descending" } else { "Ascending" }
                aria-label="Toggle sort direction" on:click=move |_| desc.update(|d| *d = !*d)>
                {move || if desc.get() { "▼" } else { "▲" }}
            </button>
            <button type="button" class="btn btn-sm btn-ghost" aria-expanded=move || show_filters.get().to_string()
                on:click=move |_| show_filters.update(|s| *s = !*s)>
                <LibIcon name="filter" />
                "Filters"
            </button>
            <ViewSwitcher mode=mode scope="series" />
            <button type="button" class="btn btn-sm" class:btn-primary=move || selecting.get() class:btn-secondary=move || !selecting.get()
                aria-pressed=move || selecting.get().to_string()
                on:click=move |_| { selecting.update(|s| *s = !*s); selected.set(Vec::new()); }>
                <LibIcon name="select" />
                "Select"
            </button>
        </div>

        <Show when=move || show_filters.get()>
            <div class="filter-panel">
                <div class="field">
                    <label for="sf-profile">"Quality profile"</label>
                    <ProfileSelect opts=opts value=f_profile id="sf-profile" none_label="Any" />
                </div>
                <div class="field">
                    <label for="sf-tag">"Tag"</label>
                    <select id="sf-tag" class="input" on:change=move |e| f_tag.set(event_target_value(&e))>
                        <option value="">"Any"</option>
                        {move || opts.tag_list().into_iter().map(|t| {
                            let id = t.id.to_string();
                            view! { <option value=id.clone() selected=move || f_tag.get() == id>{t.label}</option> }
                        }).collect_view()}
                    </select>
                </div>
                <div class="field">
                    <label for="sf-status">"Status"</label>
                    <select id="sf-status" class="input" on:change=move |e| f_status.set(event_target_value(&e))>
                        <option value="">"Any"</option>
                        {STATUSES.into_iter().map(|(s, k)| view! { <option value=k selected=move || f_status.get() == k>{series_status_label(s)}</option> }).collect_view()}
                    </select>
                </div>
                <div class="field">
                    <label for="sf-type">"Series type"</label>
                    <select id="sf-type" class="input" on:change=move |e| f_type.set(event_target_value(&e))>
                        <option value="">"Any"</option>
                        {SERIES_TYPES.iter().map(|(t, k, _)| {
                            let label = match t { SeriesType::Standard => "Standard", SeriesType::Daily => "Daily", SeriesType::Anime => "Anime" };
                            view! { <option value=*k selected=move || f_type.get() == *k>{label}</option> }
                        }).collect_view()}
                    </select>
                </div>
                <div class="field">
                    <label for="sf-network">"Network"</label>
                    <input id="sf-network" class="input" type="text" placeholder="e.g. HBO" prop:value=move || f_network.get()
                        on:change=move |e| f_network.set(event_target_value(&e)) />
                </div>
                <div class="field">
                    <label for="sf-genre">"Genre"</label>
                    <input id="sf-genre" class="input" type="text" placeholder="e.g. Drama" prop:value=move || f_genre.get()
                        on:change=move |e| f_genre.set(event_target_value(&e)) />
                </div>
                <div class="field">
                    <label for="sf-year">"Year"</label>
                    <input id="sf-year" class="input" type="number" min="1900" max="2100" prop:value=move || f_year.get()
                        on:change=move |e| f_year.set(event_target_value(&e)) />
                </div>
                <div class="filter-actions">
                    <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| {
                        f_profile.set(0); f_tag.set(String::new()); f_status.set(String::new()); f_type.set(String::new());
                        f_genre.set(String::new()); f_network.set(String::new()); f_year.set(String::new());
                    }>"Clear filters"</button>
                </div>
            </div>
        </Show>

        <p class="lib-count" aria-live="polite">{count_label}</p>
        {content}

        <Show when=move || selecting.get()>
            <BulkBar
                count=Signal::derive(move || selected.with(Vec::len))
                on_all=Callback::new(move |()| selected.set(all_ids()))
                on_none=Callback::new(move |()| selected.set(Vec::new()))
                on_done=Callback::new(move |()| { selecting.set(false); selected.set(Vec::new()); })
            >
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty) on:click=move |_| set_monitored(true)>"Monitor"</button>
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty) on:click=move |_| set_monitored(false)>"Unmonitor"</button>
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty) on:click=move |_| pass_open.set(true)>"Season pass"</button>
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty) on:click=move |_| edit_open.set(true)>
                    <LibIcon name="edit" />
                    "Edit"
                </button>
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty)
                    on:click=move |_| { let ids = selected.get_untracked(); let n = ids.len(); start_search(LibrarySearchKind::Series, ids, None, None, format!("{n} series")) }>
                    <Icon name="search" />
                    "Search"
                </button>
                <button type="button" class="btn btn-sm btn-ghost btn-danger-text" disabled=move || selected.with(Vec::is_empty) on:click=move |_| delete_open.set(true)>
                    <Icon name="trash" />
                    "Delete"
                </button>
            </BulkBar>
        </Show>

        <BulkEditModal kind=MediaKind::Series open=edit_open ids=Signal::derive(move || selected.get()) opts=opts
            on_saved=Callback::new(move |()| series.refetch()) />
        <DeleteMediaDialog open=delete_open title="Delete series?"
            message=Signal::derive(move || format!("{} series will be removed from the library.", selected.with(Vec::len)))
            on_confirm=bulk_delete />
        <Modal open=pass_open title="Season pass">
            <div class="form">
                <p class="modal-text">{move || format!("Choose which episodes of the {} selected series to monitor.", selected.with(Vec::len))}</p>
                <div class="field">
                    <label for="sp-monitor">"Monitor"</label>
                    <EnumSelect table=&SERIES_MONITOR value=pass_monitor id="sp-monitor" />
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| pass_open.set(false)>"Cancel"</button>
                    <button type="button" class="btn btn-primary" on:click=season_pass>"Apply"</button>
                </div>
            </div>
        </Modal>
    }
}
