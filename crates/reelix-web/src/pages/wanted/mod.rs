//! `/wanted/missing` and `/wanted/cutoffunmet` — paged lists of wanted movies / episodes.

use leptos::prelude::*;
use leptos_router::components::A;
use reelix_api_types::library::{CalendarKind, LibrarySearchKind, Paged, WantedItemDto};

use crate::api;
use crate::components::library_common::{Pager, Segmented, SortHeader, on_entity_changed, toggle_id};
use crate::components::library_forms::{LibOptions, require_library, start_search, with_query};
use crate::components::poster::{POSTER_SM, Poster, tmdb_image};
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, PageHeader, Skeleton};

const PAGE_SIZE: u32 = 25;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Missing,
    Cutoff,
}

#[component]
pub fn MissingPage() -> impl IntoView {
    view! { <Wanted mode=Mode::Missing /> }
}

#[component]
pub fn CutoffUnmetPage() -> impl IntoView {
    view! { <Wanted mode=Mode::Cutoff /> }
}

#[component]
fn Wanted(mode: Mode) -> impl IntoView {
    require_library();
    let opts = LibOptions::load();
    let kind = RwSignal::new("movie".to_string());
    let page = RwSignal::new(1u32);
    let sort = RwSignal::new("date".to_string());
    let desc = RwSignal::new(true);
    let include_unmonitored = RwSignal::new(false);
    let selected = RwSignal::new(Vec::<i64>::new());
    let confirm_all = RwSignal::new(false);

    // Changing what is listed resets paging and selection.
    Effect::new(move |_| {
        kind.track();
        sort.track();
        desc.track();
        include_unmonitored.track();
        page.set(1);
        selected.set(Vec::new());
    });

    let endpoint = match mode {
        Mode::Missing => "/wanted/missing",
        Mode::Cutoff => "/wanted/cutoff",
    };
    let items = LocalResource::new(move || {
        let url = with_query(
            endpoint,
            &[
                ("kind", kind.get()),
                ("page", page.get().to_string()),
                ("page_size", PAGE_SIZE.to_string()),
                ("sort", sort.get()),
                ("dir", if desc.get() { "desc" } else { "asc" }.into()),
                ("monitored", (!include_unmonitored.get()).to_string()),
            ],
        );
        async move { api::get::<Paged<WantedItemDto>>(&url).await }
    });
    on_entity_changed(
        &["movie", "episode", "movie_file", "episode_file", "series"],
        false,
        move || items.refetch(),
    );

    let media = move || {
        if kind.get_untracked() == "episode" {
            CalendarKind::Episode
        } else {
            CalendarKind::Movie
        }
    };
    let search_kind = move || match media() {
        CalendarKind::Movie => LibrarySearchKind::Movie,
        CalendarKind::Episode => LibrarySearchKind::Episode,
    };
    let total = Signal::derive(move || items.get().and_then(Result::ok).map_or(0, |p| p.total));

    let (title, subtitle, all_kind, all_label) = match mode {
        Mode::Missing => (
            "Missing",
            "Monitored items that are released but have no file yet.",
            LibrarySearchKind::Missing,
            "Search all missing",
        ),
        Mode::Cutoff => (
            "Cutoff unmet",
            "Items with a file that hasn't reached its quality profile's cutoff.",
            LibrarySearchKind::Cutoff,
            "Search all cutoff unmet",
        ),
    };

    let table = move || {
        let p = match items.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| items.refetch()) /> }
                    .into_any();
            }
            Some(Ok(p)) => p,
        };
        if p.items.is_empty() {
            let (t, d) = match mode {
                Mode::Missing => ("Nothing missing", "Every monitored, released item has a file."),
                Mode::Cutoff => ("All cutoffs met", "Every file meets its profile's cutoff."),
            };
            return view! { <EmptyState icon="check" title=t description=d compact=true /> }.into_any();
        }
        let is_episode = kind.get_untracked() == "episode";
        let ids: Vec<i64> = p.items.iter().map(|i| i.id).collect();
        let all_on = {
            let ids = ids.clone();
            move || selected.with(|s| ids.iter().all(|i| s.contains(i)))
        };
        view! {
            <div class="table-wrap">
                <table class="table">
                    <caption class="visually-hidden">{title}</caption>
                    <thead>
                        <tr>
                            <th scope="col" class="col-check">
                                <input type="checkbox" aria-label="Select page" prop:checked=all_on
                                    on:change=move |e| if event_target_checked(&e) { selected.set(ids.clone()) } else { selected.set(Vec::new()) } />
                            </th>
                            <th scope="col"><span class="visually-hidden">"Poster"</span></th>
                            <SortHeader label=if is_episode { "Series" } else { "Movie" } key="title" sort=sort desc=desc />
                            {is_episode.then(|| view! { <th scope="col">"Episode"</th> })}
                            <SortHeader label=if is_episode { "Air date" } else { "Release" } key="date" sort=sort desc=desc />
                            <th scope="col">"Profile"</th>
                            {(mode == Mode::Cutoff).then(|| view! { <th scope="col">"Current"</th><th scope="col">"Cutoff"</th> })}
                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        {p.items.into_iter().map(|it| {
                            let id = it.id;
                            let is_sel = move || selected.with(|s| s.contains(&id));
                            let href = match (it.movie_id, it.series_id) {
                                (Some(m), _) => format!("/movies/{m}"),
                                (_, Some(s)) => format!("/series/{s}"),
                                _ => "#".into(),
                            };
                            let code = match (it.season_number, it.episode_number) {
                                (Some(s), Some(e)) => format!("S{s:02}E{e:02}"),
                                _ => String::new(),
                            };
                            let what = if code.is_empty() { it.title.clone() } else { format!("{} {code}", it.title) };
                            let sk = search_kind();
                            view! {
                                <tr class:is-selected=is_sel>
                                    <td class="col-check">
                                        <input type="checkbox" aria-label=format!("Select {what}") prop:checked=is_sel
                                            on:change=move |_| toggle_id(selected, id) />
                                    </td>
                                    <td><Poster class="thumb" title=it.title.clone() url=tmdb_image(it.poster.as_deref(), POSTER_SM) /></td>
                                    <td class="cell-strong">
                                        <A href=href>{it.title.clone()}</A>
                                        {it.year.map(|y| view! { <span class="cell-sub">{y}</span> })}
                                    </td>
                                    {is_episode.then(|| view! {
                                        <td>
                                            <span class="mono">{code.clone()}</span>
                                            {it.episode_title.clone().map(|t| view! { <span class="cell-sub">{t}</span> })}
                                        </td>
                                    })}
                                    <td>{it.date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "—".into())}</td>
                                    <td>{opts.profile_name(it.quality_profile_id)}</td>
                                    {(mode == Mode::Cutoff).then(|| view! {
                                        <td>
                                            <span class="badge tone-warning">{it.quality.clone().unwrap_or_else(|| "—".into())}</span>
                                            {it.custom_format_score.map(|s| view! { <span class="cell-sub">{format!("Score {s:+}")}</span> })}
                                        </td>
                                        <td>{it.cutoff.clone().unwrap_or_else(|| "—".into())}</td>
                                    })}
                                    <td class="cell-actions">
                                        <button type="button" class="icon-btn icon-btn-sm" title=format!("Search {what}") aria-label=format!("Search {what}")
                                            on:click=move |_| start_search(sk, vec![id], None, None, what.clone())>
                                            <Icon name="search" />
                                        </button>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
        }
        .into_any()
    };

    view! {
        <PageHeader title=title subtitle=subtitle>
            <button type="button" class="btn btn-secondary" disabled=move || selected.with(Vec::is_empty)
                on:click=move |_| {
                    let ids = selected.get_untracked();
                    let n = ids.len();
                    start_search(search_kind(), ids, None, None, format!("{n} selected item(s)"));
                    selected.set(Vec::new());
                }>
                <Icon name="search" />
                {move || format!("Search selected ({})", selected.with(Vec::len))}
            </button>
            <button type="button" class="btn btn-primary" disabled=move || total.get() == 0 on:click=move |_| confirm_all.set(true)>
                <Icon name="search" />
                {all_label}
            </button>
        </PageHeader>
        <div class="lib-toolbar">
            <Segmented value=kind label="Media" options=vec![("movie", "Movies"), ("episode", "Episodes")] />
            <label class="checkbox">
                <input type="checkbox" prop:checked=move || include_unmonitored.get()
                    on:change=move |e| include_unmonitored.set(event_target_checked(&e)) />
                "Include unmonitored"
            </label>
            <span class="lib-count">{move || format!("{} item(s)", total.get())}</span>
        </div>
        <div class="card">
            <div class="card-body">
                {table}
                <Pager page=page page_size=PAGE_SIZE total=total />
            </div>
        </div>
        <ConfirmDialog open=confirm_all title=all_label
            message=Signal::derive(move || format!("Queue a search for all {} {}? This can send many queries to your indexers.",
                total.get(), if kind.get() == "episode" { "episodes" } else { "movies" }))
            confirm_label="Search all"
            on_confirm=Callback::new(move |()| {
                let m = media();
                start_search(all_kind, Vec::new(), None, Some(m), format!("all {}", if m == CalendarKind::Episode { "episodes" } else { "movies" }).to_lowercase());
            }) />
    }
}
