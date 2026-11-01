//! `/music` — the artist library: poster grid or table, filters and sort.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::music::{AlbumDto, ArtistDto, ArtistJobRequest, Paged};

use crate::api;
use crate::components::library_common::{
    LibIcon, SortHeader, ViewMode, bytes, on_entity_changed, pref_get, pref_set,
};
use crate::components::library_forms::{require_library, with_query};
use crate::components::music::{MusicOptions, artist_state, start_album_search};
use crate::components::poster::PosterCard;
use crate::components::{EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge, Tone};
use crate::state::Toasts;

const SORTS: [(&str, &str); 5] = [
    ("sort_name", "Name"),
    ("albums", "Albums"),
    ("tracks", "Tracks"),
    ("size", "Size on disk"),
    ("added", "Date added"),
];

#[component]
pub fn ArtistsPage() -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let opts = MusicOptions::load();
    let mode = RwSignal::new(match ViewMode::load("artists") {
        ViewMode::Table => ViewMode::Table,
        _ => ViewMode::Posters,
    });
    let q = RwSignal::new(String::new());
    let quick = RwSignal::new(pref_get("artists.filter").unwrap_or_else(|| "all".into()));
    let sort = RwSignal::new(pref_get("artists.sort").unwrap_or_else(|| "sort_name".into()));
    let desc = RwSignal::new(pref_get("artists.desc").as_deref() == Some("1"));

    Effect::new(move |_| {
        pref_set("artists.filter", &quick.get());
        pref_set("artists.sort", &sort.get());
        pref_set("artists.desc", if desc.get() { "1" } else { "0" });
    });

    let artists = LocalResource::new(move || {
        let mut p: Vec<(&str, String)> = vec![
            ("search", q.get().trim().to_string()),
            ("sort", sort.get()),
            ("dir", if desc.get() { "desc" } else { "asc" }.into()),
        ];
        match quick.get().as_str() {
            "monitored" => p.push(("monitored", "true".into())),
            "unmonitored" => p.push(("monitored", "false".into())),
            _ => {}
        }
        let url = with_query("/artists", &p);
        async move { api::get::<Paged<ArtistDto>>(&url).await }
    });
    on_entity_changed(&["artist", "album", "trackfile"], true, move || artists.refetch());

    let update_all = move |_| {
        spawn_local(async move {
            match api::post::<_, EnqueuedJob>("/artists/refresh", &ArtistJobRequest::default()).await {
                Ok(_) => toasts.success(
                    "Refreshing every artist in the background (MusicBrainz allows one request per second).",
                ),
                Err(e) => toasts.error(format!("Could not start the refresh: {e}")),
            }
        });
    };

    let count_label = move || {
        artists
            .get()
            .and_then(Result::ok)
            .map(|p| format!("{} artist(s)", p.total))
            .unwrap_or_default()
    };

    let content = move || {
        let items = match artists.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| artists.refetch()) /> }
                    .into_any();
            }
            Some(Ok(p)) => p.items,
        };
        if items.is_empty() {
            let filtered = quick.get_untracked() != "all" || !q.get_untracked().is_empty();
            return if filtered {
                view! {
                    <div class="card">
                        <EmptyState icon="search" title="No artists match" description="Try clearing the search or filters." />
                    </div>
                }
                .into_any()
            } else {
                view! {
                    <div class="card">
                        <EmptyState icon="music" title="Your music library is empty" description="Add artists or single albums from MusicBrainz, or import folders you already have.">
                            <A href="/music/add" attr:class="btn btn-primary"><Icon name="plus" />"Add artist"</A>
                            <A href="/music/import" attr:class="btn btn-secondary"><Icon name="folder" />"Import library"</A>
                        </EmptyState>
                    </div>
                }
                .into_any()
            };
        }
        match mode.get() {
            ViewMode::Table => view! {
                <div class="card">
                    <div class="card-body">
                        <div class="table-wrap">
                            <table class="table">
                                <caption class="visually-hidden">"Artists"</caption>
                                <thead>
                                    <tr>
                                        <th scope="col"><span class="visually-hidden">"Status"</span></th>
                                        <SortHeader label="Name" key="sort_name" sort=sort desc=desc />
                                        <SortHeader label="Albums" key="albums" sort=sort desc=desc />
                                        <SortHeader label="Tracks" key="tracks" sort=sort desc=desc />
                                        <th scope="col">"Quality profile"</th>
                                        <th scope="col">"Metadata profile"</th>
                                        <SortHeader label="Size" key="size" sort=sort desc=desc />
                                        <SortHeader label="Added" key="added" sort=sort desc=desc />
                                    </tr>
                                </thead>
                                <tbody>
                                    {items.into_iter().map(|a| {
                                        let state = artist_state(&a);
                                        let s = a.statistics.clone();
                                        let (qp, mp) = (a.quality_profile_id, a.metadata_profile_id);
                                        view! {
                                            <tr>
                                                <td><span class=format!("status-dot {}", state.class()) title=state.label()><span class="visually-hidden">{state.label()}</span></span></td>
                                                <td class="cell-strong">
                                                    <A href=format!("/music/artists/{}", a.id)>{a.name.clone()}</A>
                                                    {a.disambiguation.clone().map(|d| view! { <span class="cell-sub">{d}</span> })}
                                                </td>
                                                <td>{s.album_count}</td>
                                                <td>{format!("{} / {}", s.track_file_count, s.track_count)}</td>
                                                <td>{move || opts.profile_name(qp)}</td>
                                                <td>{move || opts.metadata_name(mp)}</td>
                                                <td>{if s.size_on_disk > 0 { bytes(s.size_on_disk) } else { "—".into() }}</td>
                                                <td>{crate::format::date(a.added)}</td>
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
            _ => view! {
                <div class="poster-grid">
                    {items.into_iter().map(|a| {
                        let s = &a.statistics;
                        view! {
                            <PosterCard
                                href=format!("/music/artists/{}", a.id)
                                title=a.name.clone()
                                subtitle=format!("{} album(s)", s.album_count)
                                poster=a.images.poster.clone()
                                state=artist_state(&a)
                                monitored=a.monitored
                                progress=(s.track_file_count, s.track_count)
                            />
                        }
                    }).collect_view()}
                </div>
            }
            .into_any(),
        }
    };

    let view_btn = move |m: ViewMode, icon: &'static str| {
        view! {
            <button type="button" class="seg-btn" class:active=move || mode.get() == m
                aria-pressed=move || (mode.get() == m).to_string() title=m.label()
                on:click=move |_| { mode.set(m); pref_set("artists.view", m.key()); }>
                <LibIcon name=icon />
                <span class="seg-label">{m.label()}</span>
            </button>
        }
    };

    view! {
        <PageHeader title="Music">
            <button type="button" class="btn btn-secondary" on:click=update_all title="Refresh metadata for every artist">
                <Icon name="refresh" />
                <span>"Update all"</span>
            </button>
            <button type="button" class="btn btn-secondary" title="Search for every missing monitored album"
                on:click=move |_| start_album_search(ContentSearchKind::Missing, Vec::new(), "all missing albums".into())>
                <Icon name="search" />
                <span>"Search missing"</span>
            </button>
            <A href="/music/add" attr:class="btn btn-primary"><Icon name="plus" />"Add artist"</A>
        </PageHeader>

        <div class="lib-toolbar">
            <input class="input grow" type="search" placeholder="Filter by name…" aria-label="Filter by name"
                prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e)) />
            <select class="input" aria-label="Show" on:change=move |e| quick.set(event_target_value(&e))>
                {[("all", "All artists"), ("monitored", "Monitored"), ("unmonitored", "Unmonitored")]
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
            <div class="segmented" role="group" aria-label="View">
                {view_btn(ViewMode::Posters, "grid")}
                {view_btn(ViewMode::Table, "table")}
            </div>
        </div>
        <p class="lib-count" aria-live="polite">{count_label}</p>
        {content}
        <WantedSummary />
    }
}

/// Small "wanted" counter below the grid.
#[component]
fn WantedSummary() -> impl IntoView {
    let wanted =
        LocalResource::new(|| api::get::<Paged<AlbumDto>>("/albums/wanted?kind=missing&page_size=1"));
    view! {
        {move || wanted.get().and_then(Result::ok).filter(|p| p.total > 0).map(|p| view! {
            <p class="lib-count">
                <StatusBadge tone=Tone::Danger label=format!("{} missing album(s)", p.total) dot=true />
            </p>
        })}
    }
}
