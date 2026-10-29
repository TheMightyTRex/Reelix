//! `/movies` — the movie library: poster / overview / table views, filters, sort and bulk edits.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::library::{
    BulkDeleteRequest, CalendarKind, LibrarySearchKind, MediaKind, MovieBulkEdit, MovieDto, MovieEdit,
    MovieStatus, Paged, Quality,
};

use crate::api;
use crate::components::library_bulk::BulkEditModal;
use crate::components::library_common::{
    BulkBar, LibIcon, SortHeader, ViewMode, ViewSwitcher, bytes, on_entity_changed, pref_get, pref_set,
    runtime, toggle_id,
};
use crate::components::library_forms::{
    DeleteMediaDialog, LibOptions, delete_json, movie_status_label, require_library, start_search, with_query,
};
use crate::components::poster::{MediaState, POSTER_MD, POSTER_SM, Poster, PosterCard, tmdb_image};
use crate::components::{EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge};
use crate::state::Toasts;

const PAGE_CHUNK: usize = 120;

pub fn movie_state(m: &MovieDto) -> MediaState {
    if !m.monitored {
        MediaState::Unmonitored
    } else if let Some(f) = &m.movie_file {
        if f.file.cutoff_met {
            MediaState::Downloaded
        } else {
            MediaState::Upgrade
        }
    } else if m.has_file {
        MediaState::Downloaded
    } else if !m.is_available {
        MediaState::Upcoming
    } else {
        MediaState::Missing
    }
}

const SORTS: [(&str, &str); 8] = [
    ("title", "Title"),
    ("year", "Year"),
    ("added", "Date added"),
    ("release", "Release date"),
    ("size", "Size on disk"),
    ("rating", "Rating"),
    ("runtime", "Runtime"),
    ("status", "Status"),
];

const STATUSES: [(MovieStatus, &str); 5] = [
    (MovieStatus::Tba, "tba"),
    (MovieStatus::Announced, "announced"),
    (MovieStatus::InCinemas, "in_cinemas"),
    (MovieStatus::Released, "released"),
    (MovieStatus::Deleted, "deleted"),
];

#[component]
pub fn MoviesPage() -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let opts = LibOptions::load();

    let mode = RwSignal::new(ViewMode::load("movies"));
    let q = RwSignal::new(String::new());
    let quick = RwSignal::new(pref_get("movies.filter").unwrap_or_else(|| "all".into()));
    let sort = RwSignal::new(pref_get("movies.sort").unwrap_or_else(|| "title".into()));
    let desc = RwSignal::new(pref_get("movies.desc").as_deref() == Some("1"));
    let show_filters = RwSignal::new(false);
    let f_profile = RwSignal::new(0i64);
    let f_quality = RwSignal::new(String::new());
    let f_tag = RwSignal::new(String::new());
    let f_genre = RwSignal::new(String::new());
    let f_year = RwSignal::new(String::new());
    let f_status = RwSignal::new(String::new());
    let shown = RwSignal::new(PAGE_CHUNK);

    Effect::new(move |_| {
        pref_set("movies.filter", &quick.get());
        pref_set("movies.sort", &sort.get());
        pref_set("movies.desc", if desc.get() { "1" } else { "0" });
    });

    let movies = LocalResource::new(move || {
        let mut p: Vec<(&str, String)> = vec![
            ("sort", sort.get()),
            ("dir", if desc.get() { "desc" } else { "asc" }.into()),
            ("q", q.get().trim().to_string()),
        ];
        match quick.get().as_str() {
            "monitored" => p.push(("monitored", "true".into())),
            "unmonitored" => p.push(("monitored", "false".into())),
            "missing" => p.push(("missing", "true".into())),
            "downloaded" => p.push(("has_file", "true".into())),
            _ => {}
        }
        let prof = f_profile.get();
        if prof != 0 {
            p.push(("quality_profile_id", prof.to_string()));
        }
        p.push(("quality", f_quality.get()));
        p.push(("tag", f_tag.get()));
        p.push(("genre", f_genre.get().trim().to_string()));
        p.push(("year", f_year.get().trim().to_string()));
        p.push(("status", f_status.get()));
        let url = with_query("/movies", &p);
        async move { api::get::<Paged<MovieDto>>(&url).await }
    });
    on_entity_changed(&["movie", "movie_file", "moviefile"], true, move || {
        movies.refetch()
    });

    // Selection
    let selecting = RwSignal::new(false);
    let selected = RwSignal::new(Vec::<i64>::new());
    let all_ids = move || {
        movies
            .get()
            .and_then(Result::ok)
            .map(|p| p.items.iter().map(|m| m.id).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let edit_open = RwSignal::new(false);
    let delete_open = RwSignal::new(false);

    let set_monitored = move |on: bool| {
        let ids = selected.get_untracked();
        let n = ids.len();
        spawn_local(async move {
            let body = MovieBulkEdit {
                ids,
                edit: MovieEdit {
                    monitored: Some(on),
                    ..Default::default()
                },
            };
            match api::put::<_, serde_json::Value>("/movies/bulk", &body).await {
                Ok(_) => {
                    toasts.success(format!(
                        "{} {n} movie(s).",
                        if on { "Monitored" } else { "Unmonitored" }
                    ));
                    movies.refetch();
                }
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };
    let bulk_delete = Callback::new(move |delete_files: bool| {
        let ids = selected.get_untracked();
        let n = ids.len();
        spawn_local(async move {
            match delete_json("/movies/bulk", &BulkDeleteRequest { ids, delete_files }).await {
                Ok(()) => {
                    toasts.success(format!("Deleted {n} movie(s)."));
                    selected.set(Vec::new());
                    movies.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });
    let update_all = move |_| {
        spawn_local(async move {
            match api::post_empty::<EnqueuedJob>("/system/tasks/refresh_movies/run").await {
                Ok(_) => toasts.success("Refreshing every movie in the background."),
                Err(e) => toasts.error(format!("Could not start the refresh: {e}")),
            }
        });
    };

    let count_label = move || {
        movies
            .get()
            .and_then(Result::ok)
            .map(|p| format!("{} movie(s)", p.total))
            .unwrap_or_default()
    };

    let content = move || {
        let page = match movies.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| movies.refetch()) /> }
                    .into_any();
            }
            Some(Ok(p)) => p,
        };
        if page.items.is_empty() {
            let filtered = quick.get_untracked() != "all" || !q.get_untracked().is_empty();
            return if filtered {
                view! {
                    <div class="card">
                        <EmptyState icon="search" title="No movies match" description="Try clearing the search or filters." />
                    </div>
                }
                .into_any()
            } else {
                view! {
                    <div class="card">
                        <EmptyState icon="movies" title="Your movie library is empty" description="Add movies from TMDB or import folders you already have.">
                            <A href="/movies/add" attr:class="btn btn-primary"><Icon name="plus" />"Add movie"</A>
                            <A href="/movies/import" attr:class="btn btn-secondary"><Icon name="folder" />"Import library"</A>
                        </EmptyState>
                    </div>
                }
                .into_any()
            };
        }
        let total = page.items.len();
        let items: Vec<MovieDto> = page.items.into_iter().take(shown.get()).collect();
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
                    {items.into_iter().map(|m| {
                        let id = m.id;
                        let quality = m.movie_file.as_ref().map(|f| f.file.quality_name.clone());
                        view! {
                            <PosterCard
                                href=format!("/movies/{id}")
                                title=m.title.clone()
                                subtitle=m.year.map(|y| y.to_string()).unwrap_or_default()
                                poster=tmdb_image(m.images.poster.as_deref(), POSTER_MD)
                                state=movie_state(&m)
                                monitored=m.monitored
                                badge=quality.unwrap_or_default()
                                selected=Signal::derive(move || selected.with(|s| s.contains(&id)))
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
                    {items.into_iter().map(|m| {
                        let id = m.id;
                        let state = movie_state(&m);
                        let navigate = navigate.clone();
                        view! {
                            <div class="overview-item lib-row" class:selected=move || selected.with(|s| s.contains(&id))
                                role="link" tabindex="0"
                                on:click={
                                    let navigate = navigate.clone();
                                    move |_| if selecting.get_untracked() { toggle_id(selected, id) } else { navigate(&format!("/movies/{id}"), Default::default()) }
                                }
                                on:keydown=move |e: leptos::ev::KeyboardEvent| if e.key() == "Enter" {
                                    if selecting.get_untracked() { toggle_id(selected, id) } else { navigate(&format!("/movies/{id}"), Default::default()) }
                                }>
                                <Poster title=m.title.clone() url=tmdb_image(m.images.poster.as_deref(), POSTER_SM) />
                                <div class="overview-body">
                                    <span class="overview-title">{m.title.clone()}{m.year.map(|y| format!(" ({y})"))}</span>
                                    <div class="meta-line">
                                        <StatusBadge tone=state.tone() label=state.label() dot=true />
                                        <span>{movie_status_label(m.status)}</span>
                                        {m.runtime_minutes.filter(|r| *r > 0).map(|r| view! { <span>{runtime(r)}</span> })}
                                        {m.certification.clone().map(|c| view! { <span>{c}</span> })}
                                        {m.studio.clone().map(|s| view! { <span>{s}</span> })}
                                        <span>{opts.profile_name(m.quality_profile_id)}</span>
                                        {m.movie_file.as_ref().map(|f| view! { <span class="badge tone-info">{f.file.quality_name.clone()}</span> })}
                                        {(m.size_on_disk > 0).then(|| view! { <span>{bytes(m.size_on_disk)}</span> })}
                                    </div>
                                    {m.overview.clone().map(|o| view! { <p class="overview-text">{o}</p> })}
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
                                <caption class="visually-hidden">"Movies"</caption>
                                <thead>
                                    <tr>
                                        {move || selecting.get().then(|| view! { <th scope="col" class="col-check"><span class="visually-hidden">"Selected"</span></th> })}
                                        <th scope="col"><span class="visually-hidden">"Status"</span></th>
                                        <SortHeader label="Title" key="title" sort=sort desc=desc />
                                        <SortHeader label="Year" key="year" sort=sort desc=desc />
                                        <th scope="col">"Profile"</th>
                                        <th scope="col">"Quality"</th>
                                        <SortHeader label="Size" key="size" sort=sort desc=desc />
                                        <SortHeader label="Added" key="added" sort=sort desc=desc />
                                        <SortHeader label="Status" key="status" sort=sort desc=desc />
                                    </tr>
                                </thead>
                                <tbody>
                                    {items.into_iter().map(|m| {
                                        let id = m.id;
                                        let state = movie_state(&m);
                                        let is_sel = move || selected.with(|s| s.contains(&id));
                                        view! {
                                            <tr class:is-selected=is_sel>
                                                {move || selecting.get().then(|| view! {
                                                    <td class="col-check">
                                                        <input type="checkbox" aria-label=format!("Select movie {id}") prop:checked=is_sel
                                                            on:change=move |_| toggle_id(selected, id) />
                                                    </td>
                                                })}
                                                <td><span class=format!("status-dot {}", state.class()) title=state.label()></span></td>
                                                <td class="cell-strong"><A href=format!("/movies/{id}")>{m.title.clone()}</A></td>
                                                <td>{m.year.map(|y| y.to_string()).unwrap_or_default()}</td>
                                                <td>{opts.profile_name(m.quality_profile_id)}</td>
                                                <td>{m.movie_file.as_ref().map(|f| f.file.quality_name.clone()).unwrap_or_else(|| "—".into())}</td>
                                                <td>{if m.size_on_disk > 0 { bytes(m.size_on_disk) } else { "—".into() }}</td>
                                                <td>{m.added.format("%Y-%m-%d").to_string()}</td>
                                                <td><StatusBadge tone=state.tone() label=state.label() /></td>
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
        <PageHeader title="Movies">
            <button type="button" class="btn btn-secondary" on:click=update_all title="Refresh metadata for every movie">
                <Icon name="refresh" />
                <span>"Update all"</span>
            </button>
            <button type="button" class="btn btn-secondary" title="Search for every missing monitored movie"
                on:click=move |_| start_search(LibrarySearchKind::Missing, Vec::new(), None, Some(CalendarKind::Movie), "all missing movies".into())>
                <Icon name="search" />
                <span>"Search missing"</span>
            </button>
            <A href="/movies/add" attr:class="btn btn-primary"><Icon name="plus" />"Add movie"</A>
        </PageHeader>

        <div class="lib-toolbar">
            <input class="input grow" type="search" placeholder="Filter by title…" aria-label="Filter by title"
                prop:value=move || q.get() on:input=move |e| { q.set(event_target_value(&e)); shown.set(PAGE_CHUNK); } />
            <select class="input" aria-label="Show" on:change=move |e| quick.set(event_target_value(&e))>
                {[("all", "All movies"), ("monitored", "Monitored"), ("unmonitored", "Unmonitored"), ("missing", "Missing"), ("downloaded", "Downloaded")]
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
            <button type="button" class="btn btn-sm btn-ghost" class:active=move || show_filters.get()
                aria-expanded=move || show_filters.get().to_string() on:click=move |_| show_filters.update(|s| *s = !*s)>
                <LibIcon name="filter" />
                "Filters"
            </button>
            <ViewSwitcher mode=mode scope="movies" />
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
                    <label for="mf-profile">"Quality profile"</label>
                    <crate::components::library_forms::ProfileSelect opts=opts value=f_profile id="mf-profile" none_label="Any" />
                </div>
                <div class="field">
                    <label for="mf-quality">"File quality"</label>
                    <select id="mf-quality" class="input" on:change=move |e| f_quality.set(event_target_value(&e))>
                        <option value="">"Any"</option>
                        {Quality::ALL.iter().filter(|q| q.id() != 0).map(|q| {
                            let id = q.id().to_string();
                            view! { <option value=id.clone() selected=move || f_quality.get() == id>{q.name()}</option> }
                        }).collect_view()}
                    </select>
                </div>
                <div class="field">
                    <label for="mf-tag">"Tag"</label>
                    <select id="mf-tag" class="input" on:change=move |e| f_tag.set(event_target_value(&e))>
                        <option value="">"Any"</option>
                        {move || opts.tag_list().into_iter().map(|t| {
                            let id = t.id.to_string();
                            view! { <option value=id.clone() selected=move || f_tag.get() == id>{t.label}</option> }
                        }).collect_view()}
                    </select>
                </div>
                <div class="field">
                    <label for="mf-status">"Status"</label>
                    <select id="mf-status" class="input" on:change=move |e| f_status.set(event_target_value(&e))>
                        <option value="">"Any"</option>
                        {STATUSES.into_iter().map(|(s, k)| view! { <option value=k selected=move || f_status.get() == k>{movie_status_label(s)}</option> }).collect_view()}
                    </select>
                </div>
                <div class="field">
                    <label for="mf-genre">"Genre"</label>
                    <input id="mf-genre" class="input" type="text" placeholder="e.g. Drama" prop:value=move || f_genre.get()
                        on:change=move |e| f_genre.set(event_target_value(&e)) />
                </div>
                <div class="field">
                    <label for="mf-year">"Year"</label>
                    <input id="mf-year" class="input" type="number" min="1870" max="2100" prop:value=move || f_year.get()
                        on:change=move |e| f_year.set(event_target_value(&e)) />
                </div>
                <div class="filter-actions">
                    <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| {
                        f_profile.set(0); f_quality.set(String::new()); f_tag.set(String::new());
                        f_genre.set(String::new()); f_year.set(String::new()); f_status.set(String::new());
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
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty) on:click=move |_| set_monitored(true)>
                    "Monitor"
                </button>
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty) on:click=move |_| set_monitored(false)>
                    "Unmonitor"
                </button>
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty) on:click=move |_| edit_open.set(true)>
                    <LibIcon name="edit" />
                    "Edit"
                </button>
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || selected.with(Vec::is_empty)
                    on:click=move |_| { let ids = selected.get_untracked(); let n = ids.len(); start_search(LibrarySearchKind::Movie, ids, None, None, format!("{n} movie(s)")) }>
                    <Icon name="search" />
                    "Search"
                </button>
                <button type="button" class="btn btn-sm btn-ghost btn-danger-text" disabled=move || selected.with(Vec::is_empty) on:click=move |_| delete_open.set(true)>
                    <Icon name="trash" />
                    "Delete"
                </button>
            </BulkBar>
        </Show>

        <BulkEditModal kind=MediaKind::Movie open=edit_open ids=Signal::derive(move || selected.get()) opts=opts
            on_saved=Callback::new(move |()| movies.refetch()) />
        <DeleteMediaDialog open=delete_open title="Delete movies?"
            message=Signal::derive(move || format!("{} movie(s) will be removed from the library.", selected.with(Vec::len)))
            on_confirm=bulk_delete />
    }
}
