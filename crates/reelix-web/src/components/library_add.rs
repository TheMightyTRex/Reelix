//! "Add movie" / "Add series": TMDB lookup with in-library state and the add modal.

use std::time::Duration;

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_query_map};
use reelix_api_types::library::{
    AddMovieRequest, AddSeriesRequest, LookupResult, MediaKind, MinimumAvailability, MovieDto, MovieMonitor,
    NewItemMonitor, SeriesDto, SeriesMonitor, SeriesType,
};

use super::library_common::{pref_get, pref_set};
use super::library_forms::{
    EnumSelect, LibOptions, MIN_AVAILABILITY, MOVIE_MONITOR, ProfileSelect, RootFolderSelect, SERIES_MONITOR,
    SERIES_TYPES, TagPicker, enc, opt_from, opt_key,
};
use super::poster::{POSTER_SM, Poster, tmdb_image};
use super::{EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton, Spinner};
use crate::api;
use crate::state::Toasts;

fn base(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movies",
        MediaKind::Series => "series",
    }
}

/// The full add page for one media kind. Accepts `?term=` to prefill the search.
#[component]
pub fn AddMediaPage(kind: MediaKind) -> impl IntoView {
    let query = use_query_map();
    let initial = query.with_untracked(|q| q.get("term").unwrap_or_default());
    let input = RwSignal::new(initial.clone());
    let term = RwSignal::new(initial);
    let opts = LibOptions::load();
    let target = RwSignal::new(None::<LookupResult>);
    let modal_open = RwSignal::new(false);

    // Debounce typing into the actual lookup term.
    let pending = StoredValue::new(0u32);
    let on_input = move |e: ev::Event| {
        let v = event_target_value(&e);
        input.set(v.clone());
        let n = pending.get_value() + 1;
        pending.set_value(n);
        set_timeout(
            move || {
                if pending.try_get_value() == Some(n) {
                    let _ = term.try_set(v.trim().to_string());
                }
            },
            Duration::from_millis(450),
        );
    };

    let results = LocalResource::new(move || {
        let t = term.get();
        async move {
            if t.trim().len() < 2 {
                None
            } else {
                Some(api::get::<Vec<LookupResult>>(&format!("/{}/lookup?term={}", base(kind), enc(&t))).await)
            }
        }
    });

    let (title, noun, placeholder) = match kind {
        MediaKind::Movie => (
            "Add movie",
            "movie",
            "Search by title, or tmdb:603 / imdb:tt0133093",
        ),
        MediaKind::Series => (
            "Add series",
            "series",
            "Search by title, or tmdb:1396 / tvdb:81189",
        ),
    };

    let list = move || {
        match results.get() {
        None => view! { <Skeleton lines=4 /> }.into_any(),
        Some(None) => view! {
            <div class="card">
                <EmptyState icon="search" title=format!("Find a {noun}")
                    description="Results come from TMDB. Items already in your library are marked." />
            </div>
        }
        .into_any(),
        Some(Some(Err(e))) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| results.refetch()) /> }.into_any(),
        Some(Some(Ok(list))) if list.is_empty() => view! {
            <div class="card">
                <EmptyState icon="search" title="No matches" description="Try a different spelling, the original title or an id like tmdb:12345." />
            </div>
        }
        .into_any(),
        Some(Some(Ok(list))) => view! {
            <div class="lookup-list">
                {list.into_iter().map(|r| {
                    let year = r.year.map(|y| format!(" ({y})")).unwrap_or_default();
                    let poster = tmdb_image(r.poster.as_deref(), POSTER_SM);
                    let action = match (r.in_library, r.library_id) {
                        (true, Some(id)) => view! {
                            <A href=format!("/{}/{id}", base(kind)) attr:class="btn btn-sm btn-secondary">
                                <Icon name="check" />
                                "In library"
                            </A>
                        }.into_any(),
                        (true, None) => view! { <span class="badge tone-success">"In library"</span> }.into_any(),
                        _ => {
                            let r2 = r.clone();
                            view! {
                                <button type="button" class="btn btn-sm btn-primary" on:click=move |_| {
                                    target.set(Some(r2.clone()));
                                    modal_open.set(true);
                                }>
                                    <Icon name="plus" />
                                    "Add"
                                </button>
                            }.into_any()
                        }
                    };
                    view! {
                        <article class="lookup-item">
                            <Poster title=r.title.clone() url=poster />
                            <div class="overview-body">
                                <span class="overview-title">{format!("{}{year}", r.title)}</span>
                                <div class="meta-line">
                                    {r.original_title.clone().filter(|o| *o != r.title).map(|o| view! { <span>{o}</span> })}
                                    {(r.vote_average > 0.0).then(|| view! { <span class="rating"><Icon name="star" class="icon-fill" />{format!("{:.1}", r.vote_average)}</span> })}
                                    {r.tmdb_id.map(|id| view! { <span>{format!("TMDB {id}")}</span> })}
                                </div>
                                {r.overview.clone().map(|o| view! { <p class="overview-text">{o}</p> })}
                            </div>
                            <div>{action}</div>
                        </article>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title=title subtitle=format!("Search TMDB and add a {noun} to your library.") />
        <div class="lib-toolbar">
            <div class="search add-search grow">
                <label for="add-term" class="visually-hidden">"Search"</label>
                <input id="add-term" class="input grow" type="search" placeholder=placeholder autofocus
                    prop:value=move || input.get() on:input=on_input
                    on:keydown=move |e: ev::KeyboardEvent| if e.key() == "Enter" { term.set(input.get_untracked().trim().to_string()) } />
            </div>
        </div>
        {list}
        <AddMediaModal kind=kind open=modal_open target=target opts=opts />
    }
}

/// The add dialog (also used from collections and the import screen's "add one").
#[component]
pub fn AddMediaModal(
    kind: MediaKind,
    open: RwSignal<bool>,
    target: RwSignal<Option<LookupResult>>,
    opts: LibOptions,
    #[prop(optional)] on_added: Option<Callback<i64>>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let scope = base(kind);
    let saved = |k: &str| pref_get(&format!("{scope}.add.{k}"));
    let root = RwSignal::new(saved("root").and_then(|v| v.parse().ok()).unwrap_or(0i64));
    let profile = RwSignal::new(saved("profile").and_then(|v| v.parse().ok()).unwrap_or(0i64));
    let tags = RwSignal::new(Vec::<i64>::new());
    let search = RwSignal::new(saved("search").is_none_or(|v| v == "1"));
    let movie_monitor = RwSignal::new(
        saved("monitor")
            .and_then(|v| opt_from(&MOVIE_MONITOR, &v))
            .unwrap_or(MovieMonitor::MovieOnly),
    );
    let min_avail = RwSignal::new(
        saved("minavail")
            .and_then(|v| opt_from(&MIN_AVAILABILITY, &v))
            .unwrap_or(MinimumAvailability::Released),
    );
    let series_monitor = RwSignal::new(
        saved("monitor")
            .and_then(|v| opt_from(&SERIES_MONITOR, &v))
            .unwrap_or(SeriesMonitor::All),
    );
    let series_type = RwSignal::new(SeriesType::Standard);
    let season_folder = RwSignal::new(saved("seasonfolder").is_none_or(|v| v == "1"));
    let cutoff_search = RwSignal::new(false);
    let busy = RwSignal::new(false);

    // Reset per-item choices whenever the dialog opens.
    Effect::new(move |_| {
        if open.get() {
            series_type.set(SeriesType::Standard);
            tags.set(Vec::new());
        }
    });

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(t) = target.get_untracked() else { return };
        if root.get_untracked() == 0 {
            toasts.error("Add a root folder first (Settings → Media Management).");
            return;
        }
        let remember = move |k: &str, v: &str| pref_set(&format!("{scope}.add.{k}"), v);
        remember("root", &root.get_untracked().to_string());
        remember("profile", &profile.get_untracked().to_string());
        remember("search", if search.get_untracked() { "1" } else { "0" });
        busy.set(true);
        let navigate = navigate.clone();
        spawn_local(async move {
            let res = match kind {
                MediaKind::Movie => {
                    remember("monitor", opt_key(&MOVIE_MONITOR, movie_monitor.get_untracked()));
                    remember("minavail", opt_key(&MIN_AVAILABILITY, min_avail.get_untracked()));
                    let req = AddMovieRequest {
                        tmdb_id: t.tmdb_id.unwrap_or_default(),
                        quality_profile_id: profile.get_untracked(),
                        root_folder_id: root.get_untracked(),
                        monitor: movie_monitor.get_untracked(),
                        minimum_availability: min_avail.get_untracked(),
                        tags: tags.get_untracked(),
                        search_on_add: search.get_untracked(),
                        path: None,
                        added_by_request_id: None,
                    };
                    api::post::<_, MovieDto>("/movies", &req).await.map(|m| m.id)
                }
                MediaKind::Series => {
                    remember(
                        "monitor",
                        opt_key(&SERIES_MONITOR, series_monitor.get_untracked()),
                    );
                    remember(
                        "seasonfolder",
                        if season_folder.get_untracked() { "1" } else { "0" },
                    );
                    let req = AddSeriesRequest {
                        tmdb_id: t.tmdb_id,
                        tvdb_id: t.tvdb_id,
                        quality_profile_id: profile.get_untracked(),
                        root_folder_id: root.get_untracked(),
                        monitor: series_monitor.get_untracked(),
                        seasons: None,
                        series_type: series_type.get_untracked(),
                        season_folder: season_folder.get_untracked(),
                        monitor_new_items: NewItemMonitor::All,
                        tags: tags.get_untracked(),
                        search_on_add: search.get_untracked(),
                        search_cutoff_unmet: cutoff_search.get_untracked(),
                        path: None,
                        added_by_request_id: None,
                    };
                    api::post::<_, SeriesDto>("/series", &req).await.map(|s| s.id)
                }
            };
            busy.set(false);
            match res {
                Ok(id) => {
                    toasts.success(format!("Added “{}”.", t.title));
                    open.set(false);
                    match on_added {
                        Some(cb) => cb.run(id),
                        None => navigate(&format!("/{scope}/{id}"), Default::default()),
                    }
                }
                Err(e) => toasts.error(format!("Could not add: {e}")),
            }
        });
    };

    let modal_title = match kind {
        MediaKind::Movie => "Add movie",
        MediaKind::Series => "Add series",
    };

    view! {
        <Modal open=open title=modal_title>
            <form class="form" on:submit=submit.clone()>
                {move || target.get().map(|t| view! {
                    <div class="add-head">
                        <Poster title=t.title.clone() url=tmdb_image(t.poster.as_deref(), POSTER_SM) />
                        <div class="overview-body">
                            <span class="overview-title">
                                {t.title.clone()}
                                {t.year.map(|y| format!(" ({y})"))}
                            </span>
                            {t.overview.clone().map(|o| view! { <p class="overview-text">{o}</p> })}
                        </div>
                    </div>
                })}
                <div class="field">
                    <label for="add-root">"Root folder"</label>
                    <RootFolderSelect opts=opts kind=kind value=root id="add-root" />
                    {move || opts.root_list(kind).is_empty().then(|| view! {
                        <p class="field-hint text-danger">
                            "No root folders for this media type. "
                            <A href="/settings/mediamanagement">"Add one in Media Management"</A>
                        </p>
                    })}
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="add-profile">"Quality profile"</label>
                        <ProfileSelect opts=opts value=profile id="add-profile" />
                    </div>
                    {match kind {
                        MediaKind::Movie => view! {
                            <div class="field">
                                <label for="add-minavail">"Minimum availability"</label>
                                <EnumSelect table=&MIN_AVAILABILITY value=min_avail id="add-minavail" />
                            </div>
                        }.into_any(),
                        MediaKind::Series => view! {
                            <div class="field">
                                <label for="add-type">"Series type"</label>
                                <EnumSelect table=&SERIES_TYPES value=series_type id="add-type" />
                            </div>
                        }.into_any(),
                    }}
                </div>
                <div class="field">
                    <label for="add-monitor">"Monitor"</label>
                    {match kind {
                        MediaKind::Movie => view! { <EnumSelect table=&MOVIE_MONITOR value=movie_monitor id="add-monitor" /> }.into_any(),
                        MediaKind::Series => view! { <EnumSelect table=&SERIES_MONITOR value=series_monitor id="add-monitor" /> }.into_any(),
                    }}
                </div>
                <div class="field">
                    <span class="field-label">"Tags"</span>
                    <TagPicker opts=opts value=tags />
                </div>
                {(kind == MediaKind::Series).then(|| view! {
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || season_folder.get()
                            on:change=move |e| season_folder.set(event_target_checked(&e)) />
                        "Use season folders"
                    </label>
                })}
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || search.get() on:change=move |e| search.set(event_target_checked(&e)) />
                    {match kind {
                        MediaKind::Movie => "Start searching for the movie",
                        MediaKind::Series => "Start searching for missing episodes",
                    }}
                </label>
                {(kind == MediaKind::Series).then(|| view! {
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || cutoff_search.get()
                            on:change=move |e| cutoff_search.set(event_target_checked(&e)) />
                        "Also search for cutoff-unmet episodes"
                    </label>
                })}
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="plus" /> }.into_any() }}
                        {modal_title}
                    </button>
                </div>
            </form>
        </Modal>
    }
}
