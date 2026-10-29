//! `/music/artists/{id}` — one artist: header, actions and albums grouped by type (Album, EP,
//! Single, Live, …) with monitor toggles and file progress.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::music::{AlbumDto, ArtistDto, ArtistEdit, NewAlbumMonitor, Paged};

use crate::api;
use crate::components::library_common::{LibIcon, bytes, on_entity_changed};
use crate::components::library_forms::{DeleteMediaDialog, fire, require_library};
use crate::components::music::{
    ArtistEditModal, MusicOptions, MusicRenameModal, album_group, album_state, artist_state,
    set_albums_monitored, start_album_search,
};
use crate::components::poster::{EpisodeProgress, Poster};
use crate::components::{EmptyState, ErrorBox, Icon, Skeleton, StatusBadge};
use crate::state::Toasts;

#[component]
pub fn ArtistDetailPage() -> impl IntoView {
    require_library();
    let params = use_params_map();
    let id = move || params.with(|p| p.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0));
    view! { {move || view! { <ArtistDetail id=id() /> }} }
}

/// Albums grouped by display type, studio albums first, newest first within a group.
fn group_albums(mut albums: Vec<AlbumDto>) -> Vec<(String, Vec<AlbumDto>)> {
    albums.sort_by(|a, b| {
        b.release_date
            .cmp(&a.release_date)
            .then_with(|| a.title.cmp(&b.title))
    });
    let mut groups: Vec<(String, Vec<AlbumDto>)> = Vec::new();
    for a in albums {
        let g = album_group(&a);
        match groups.iter_mut().find(|(k, _)| *k == g) {
            Some((_, list)) => list.push(a),
            None => groups.push((g, vec![a])),
        }
    }
    let rank = |g: &str| match g {
        "Album" => 0,
        "EP" => 1,
        "Single" => 2,
        _ => 3,
    };
    groups.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0)));
    groups
}

#[component]
fn ArtistDetail(id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let opts = MusicOptions::load();
    let artist =
        LocalResource::new(move || async move { api::get::<ArtistDto>(&format!("/artists/{id}")).await });
    let albums = LocalResource::new(move || async move {
        api::get::<Paged<AlbumDto>>(&format!("/albums?artist_id={id}")).await
    });
    on_entity_changed(&["artist", "album", "trackfile"], true, move || {
        artist.refetch();
        albums.refetch();
    });

    let edit_open = RwSignal::new(false);
    let rename_open = RwSignal::new(false);
    let delete_open = RwSignal::new(false);
    let artist_sig = Signal::derive(move || artist.get().and_then(Result::ok));
    let today = chrono::Utc::now().date_naive();

    let toggle_artist = move |a: ArtistDto| {
        let edit = ArtistEdit {
            monitored: Some(!a.monitored),
            ..Default::default()
        };
        spawn_local(async move {
            match api::put::<_, ArtistDto>(&format!("/artists/{}", a.id), &edit).await {
                Ok(_) => artist.refetch(),
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };

    let delete = Callback::new(move |delete_files: bool| {
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::delete(&format!("/artists/{id}?delete_files={delete_files}")).await {
                Ok(()) => {
                    toasts.success("Artist deleted.");
                    navigate("/music", Default::default());
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    let header = move || {
        let a = match artist.get() {
            None => return view! { <div class="detail-hero"><Skeleton lines=5 /></div> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| artist.refetch()) /> }
                    .into_any();
            }
            Some(Ok(a)) => a,
        };
        let state = artist_state(&a);
        let s = a.statistics.clone();
        let name = a.name.clone();
        let a_mon = a.clone();
        let tags = opts.tag_names(&a.tags);
        let (qp, mp) = (a.quality_profile_id, a.metadata_profile_id);
        view! {
            <div class="detail-hero">
                <div class="detail-inner">
                    <Poster title=a.name.clone() url=a.images.poster.clone() />
                    <div class="detail-info">
                        <h1 class="detail-title">{a.name.clone()}</h1>
                        {a.disambiguation.clone().map(|d| view! { <p class="muted">{d}</p> })}
                        <div class="meta-line">
                            <StatusBadge tone=state.tone() label=state.label() dot=true />
                            {a.artist_type.clone().map(|t| view! { <span>{t}</span> })}
                            {a.country.clone().map(|c| view! { <span>{c}</span> })}
                            <span>{format!("{} album(s)", s.album_count)}</span>
                            <span>{format!("{} / {} tracks", s.track_file_count, s.track_count)}</span>
                            {(s.size_on_disk > 0).then(|| view! { <span>{bytes(s.size_on_disk)}</span> })}
                        </div>
                        {(!a.genres.is_empty()).then(|| view! {
                            <div class="chip-list">{a.genres.iter().take(8).map(|g| view! { <span class="chip">{g.clone()}</span> }).collect_view()}</div>
                        })}
                        {a.overview.clone().map(|o| view! { <p class="detail-overview">{o}</p> })}
                        <div class="detail-facts">
                            <div class="fact"><span class="fact-label">"Path"</span><span class="mono">{a.path.clone()}</span></div>
                            <div class="fact"><span class="fact-label">"Quality profile"</span><span>{move || opts.profile_name(qp)}</span></div>
                            <div class="fact"><span class="fact-label">"Metadata profile"</span><span>{move || opts.metadata_name(mp)}</span></div>
                            <div class="fact"><span class="fact-label">"New albums"</span>
                                <span>{match a.monitor_new_items {
                                    NewAlbumMonitor::All => "Monitored",
                                    NewAlbumMonitor::New => "Monitored when released from now on",
                                    NewAlbumMonitor::None => "Not monitored",
                                }}</span>
                            </div>
                            <div class="fact"><span class="fact-label">"MusicBrainz"</span>
                                <a href=format!("https://musicbrainz.org/artist/{}", a.musicbrainz_id) target="_blank" rel="noopener noreferrer">{a.musicbrainz_id.clone()}</a>
                            </div>
                            {(!tags.is_empty()).then(|| view! {
                                <div class="fact"><span class="fact-label">"Tags"</span>
                                    <span class="chip-list">{tags.into_iter().map(|t| view! { <span class="chip">{t}</span> }).collect_view()}</span>
                                </div>
                            })}
                        </div>
                        <div class="detail-actions">
                            <button type="button" class="btn btn-primary"
                                on:click=move |_| start_album_search(ContentSearchKind::Parent, vec![id], name.clone())>
                                <Icon name="search" />
                                "Search monitored"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| fire(format!("/artists/{id}/refresh"), "Refresh queued.".into())>
                                <Icon name="refresh" />
                                "Refresh"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| fire(format!("/artists/{id}/rescan"), "Disk scan queued.".into())>
                                <Icon name="folder" />
                                "Rescan"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| rename_open.set(true)>
                                <LibIcon name="rename" />
                                "Rename"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| toggle_artist(a_mon.clone())>
                                <LibIcon name=if a.monitored { "bookmark-off" } else { "bookmark" } />
                                {if a.monitored { "Unmonitor" } else { "Monitor" }}
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| edit_open.set(true)>
                                <LibIcon name="edit" />
                                "Edit"
                            </button>
                            <button type="button" class="btn btn-ghost btn-danger-text" on:click=move |_| delete_open.set(true)>
                                <Icon name="trash" />
                                "Delete"
                            </button>
                        </div>
                    </div>
                </div>
            </div>
        }
        .into_any()
    };

    let album_list = move || {
        let list = match albums.get() {
            None => return view! { <Skeleton lines=4 /> }.into_any(),
            Some(Err(e)) => return view! { <ErrorBox error=e /> }.into_any(),
            Some(Ok(p)) => p.items,
        };
        if list.is_empty() {
            return view! {
                <div class="card">
                    <EmptyState icon="music" title="No albums yet"
                        description="Refresh the artist to load their albums from MusicBrainz, or change the metadata profile to include more album types." />
                </div>
            }
            .into_any();
        }
        group_albums(list)
            .into_iter()
            .map(|(group, items)| {
                let n = items.len();
                let ids: Vec<i64> = items.iter().map(|a| a.id).collect();
                let all_on = items.iter().all(|a| a.monitored);
                view! {
                    <section class="card">
                        <header class="card-header">
                            <div>
                                <h2 class="card-title">{group.clone()}</h2>
                                <p class="card-subtitle">{format!("{n} release group(s)")}</p>
                            </div>
                            <button type="button" class="btn btn-sm btn-ghost"
                                on:click=move |_| set_albums_monitored(ids.clone(), !all_on, move || albums.refetch())>
                                <LibIcon name=if all_on { "bookmark-off" } else { "bookmark" } />
                                {if all_on { "Unmonitor all" } else { "Monitor all" }}
                            </button>
                        </header>
                        <div class="card-body">
                            <div class="table-wrap">
                                <table class="table">
                                    <caption class="visually-hidden">{group}</caption>
                                    <thead>
                                        <tr>
                                            <th scope="col"><span class="visually-hidden">"Monitored"</span></th>
                                            <th scope="col">"Title"</th>
                                            <th scope="col">"Released"</th>
                                            <th scope="col">"Tracks"</th>
                                            <th scope="col">"Size"</th>
                                            <th scope="col">"Status"</th>
                                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {items.into_iter().map(|a| {
                                            let aid = a.id;
                                            let on = a.monitored;
                                            let state = album_state(&a, today);
                                            let s = a.statistics.clone();
                                            let title = a.title.clone();
                                            view! {
                                                <tr>
                                                    <td>
                                                        <button type="button" class="icon-btn"
                                                            title=if on { "Monitored — click to unmonitor" } else { "Unmonitored — click to monitor" }
                                                            aria-label=format!("{} {}", if on { "Unmonitor" } else { "Monitor" }, a.title)
                                                            on:click=move |_| set_albums_monitored(vec![aid], !on, move || albums.refetch())>
                                                            <LibIcon name=if on { "bookmark" } else { "bookmark-off" } />
                                                        </button>
                                                    </td>
                                                    <td class="cell-strong">
                                                        <A href=format!("/music/albums/{aid}")>{a.title.clone()}</A>
                                                        {a.disambiguation.clone().map(|d| view! { <span class="cell-sub">{d}</span> })}
                                                    </td>
                                                    <td>{a.release_date.map(|d| d.to_string()).unwrap_or_else(|| "—".into())}</td>
                                                    <td><EpisodeProgress have=s.track_file_count total=s.track_count compact=true /></td>
                                                    <td>{if s.size_on_disk > 0 { bytes(s.size_on_disk) } else { "—".into() }}</td>
                                                    <td><StatusBadge tone=state.tone() label=state.label() /></td>
                                                    <td class="cell-actions">
                                                        {on.then(|| view! {
                                                            <button type="button" class="icon-btn" title="Search" aria-label=format!("Search {title}")
                                                                on:click=move |_| start_album_search(ContentSearchKind::Item, vec![aid], title.clone())>
                                                                <Icon name="search" />
                                                            </button>
                                                        })}
                                                        <A href=format!("/music/albums/{aid}") attr:class="btn btn-sm btn-ghost">"Details"</A>
                                                    </td>
                                                </tr>
                                            }
                                        }).collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        </div>
                    </section>
                }
            })
            .collect_view()
            .into_any()
    };

    view! {
        {header}
        {album_list}
        <ArtistEditModal open=edit_open artist=artist_sig opts=opts on_saved=Callback::new(move |()| { artist.refetch(); albums.refetch(); }) />
        <MusicRenameModal open=rename_open artist_id=id />
        <DeleteMediaDialog open=delete_open title="Delete artist?"
            message=Signal::derive(move || artist.get().and_then(Result::ok).map(|a| format!("“{}” and all their albums will be removed from the library.", a.name)).unwrap_or_default())
            on_confirm=delete />
    }
}
