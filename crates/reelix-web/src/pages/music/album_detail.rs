//! `/music/albums/{id}` — one album: the chosen release (with a release chooser), the
//! tracklist with file status, and the album's track files.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::music::{AlbumDto, AlbumEdit, TrackDto, TrackFileDto};

use crate::api;
use crate::components::interactive_search::{InteractiveSearchModal, SearchTarget};
use crate::components::library_common::{LibIcon, bytes, on_entity_changed};
use crate::components::library_forms::{DeleteMediaDialog, require_library};
use crate::components::music::{
    ChooseReleaseModal, album_state, album_type_label, duration, secondary_label, start_album_search,
    track_status,
};
use crate::components::poster::{EpisodeProgress, Poster};
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, Skeleton, StatusBadge};
use crate::state::Toasts;

#[component]
pub fn AlbumDetailPage() -> impl IntoView {
    require_library();
    let params = use_params_map();
    let id = move || params.with(|p| p.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0));
    view! { {move || view! { <AlbumDetail id=id() /> }} }
}

#[component]
fn AlbumDetail(id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let album =
        LocalResource::new(move || async move { api::get::<AlbumDto>(&format!("/albums/{id}")).await });
    let tracks = LocalResource::new(move || async move {
        api::get::<Vec<TrackDto>>(&format!("/tracks?album_id={id}")).await
    });
    let files = LocalResource::new(move || async move {
        api::get::<Vec<TrackFileDto>>(&format!("/trackfiles?album_id={id}")).await
    });
    let refetch = move || {
        album.refetch();
        tracks.refetch();
        files.refetch();
    };
    on_entity_changed(&["album", "trackfile"], true, refetch);

    let release_open = RwSignal::new(false);
    let isearch_open = RwSignal::new(false);
    let isearch_target = RwSignal::new(None::<SearchTarget>);
    let delete_open = RwSignal::new(false);
    let file_to_delete = RwSignal::new(None::<TrackFileDto>);
    let file_delete_open = RwSignal::new(false);
    let album_sig = Signal::derive(move || album.get().and_then(Result::ok));
    let today = chrono::Utc::now().date_naive();

    let edit = move |e: AlbumEdit| {
        spawn_local(async move {
            match api::put::<_, AlbumDto>(&format!("/albums/{id}"), &e).await {
                Ok(_) => album.refetch(),
                Err(err) => toasts.error(err.to_string()),
            }
        });
    };

    let delete = Callback::new(move |delete_files: bool| {
        let navigate = navigate.clone();
        let artist = album.get_untracked().and_then(Result::ok).map(|a| a.artist_id);
        spawn_local(async move {
            match api::delete(&format!("/albums/{id}?delete_files={delete_files}")).await {
                Ok(()) => {
                    toasts.success("Album deleted.");
                    let to = artist.map_or_else(|| "/music".to_string(), |a| format!("/music/artists/{a}"));
                    navigate(&to, Default::default());
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });
    let delete_file = Callback::new(move |()| {
        let Some(f) = file_to_delete.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/trackfiles/{}", f.id)).await {
                Ok(()) => {
                    toasts.success("File deleted.");
                    refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    let header = move || {
        let a = match album.get() {
            None => return view! { <div class="detail-hero"><Skeleton lines=5 /></div> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| album.refetch()) /> }
                    .into_any();
            }
            Some(Ok(a)) => a,
        };
        let state = album_state(&a, today);
        let s = a.statistics.clone();
        let release = a.releases.iter().find(|r| r.selected).cloned();
        let title = a.title.clone();
        let is_title = format!("{} - {}", a.artist_name, a.title);
        let (on, any_ok) = (a.monitored, a.any_release_ok);
        let kind = std::iter::once(album_type_label(a.album_type).to_string())
            .chain(a.secondary_types.iter().map(|t| secondary_label(*t).to_string()))
            .collect::<Vec<_>>()
            .join(" · ");
        view! {
            <div class="detail-hero">
                <div class="detail-inner">
                    <Poster title=a.title.clone() url=a.images.poster.clone() />
                    <div class="detail-info">
                        <h1 class="detail-title">{a.title.clone()}</h1>
                        <p class="muted">
                            <A href=format!("/music/artists/{}", a.artist_id)>{a.artist_name.clone()}</A>
                            {a.disambiguation.clone().map(|d| format!(" · {d}"))}
                        </p>
                        <div class="meta-line">
                            <StatusBadge tone=state.tone() label=state.label() dot=true />
                            <span>{kind}</span>
                            {a.release_date.map(|d| view! { <span>{d.to_string()}</span> })}
                            {(s.size_on_disk > 0).then(|| view! { <span>{bytes(s.size_on_disk)}</span> })}
                        </div>
                        <EpisodeProgress have=s.track_file_count total=s.track_count />
                        {(!a.genres.is_empty()).then(|| view! {
                            <div class="chip-list">{a.genres.iter().take(8).map(|g| view! { <span class="chip">{g.clone()}</span> }).collect_view()}</div>
                        })}
                        <div class="detail-facts">
                            <div class="fact">
                                <span class="fact-label">"Release"</span>
                                <span>
                                    {match &release {
                                        Some(r) => view! {
                                            <span>
                                                {r.title.clone()}
                                                <span class="cell-sub">{[
                                                    r.format.clone(),
                                                    r.country.clone(),
                                                    r.release_date.map(|d| d.to_string()),
                                                    r.label.clone(),
                                                    Some(format!("{} tracks", r.track_count)),
                                                ].into_iter().flatten().collect::<Vec<_>>().join(" · ")}</span>
                                            </span>
                                        }.into_any(),
                                        None => view! { <span class="muted">"Not chosen yet"</span> }.into_any(),
                                    }}
                                </span>
                            </div>
                            <div class="fact">
                                <span class="fact-label">"Import"</span>
                                <label class="checkbox">
                                    <input type="checkbox" prop:checked=any_ok
                                        on:change=move |e| edit(AlbumEdit { any_release_ok: Some(event_target_checked(&e)), ..Default::default() }) />
                                    "Accept any release of this album"
                                </label>
                            </div>
                            <div class="fact"><span class="fact-label">"MusicBrainz"</span>
                                <a href=format!("https://musicbrainz.org/release-group/{}", a.musicbrainz_release_group_id) target="_blank" rel="noopener noreferrer">{a.musicbrainz_release_group_id.clone()}</a>
                            </div>
                        </div>
                        <div class="detail-actions">
                            <button type="button" class="btn btn-primary" disabled=!on
                                title=if on { "Search for this album" } else { "Monitor the album to search for it" }
                                on:click=move |_| start_album_search(ContentSearchKind::Item, vec![id], title.clone())>
                                <Icon name="search" />
                                "Search"
                            </button>
                            // INTERACTIVE_SEARCH
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| { isearch_target.set(Some(SearchTarget::album(id, is_title.clone()))); isearch_open.set(true); }>
                                <Icon name="list" />
                                "Interactive"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| release_open.set(true)>
                                <LibIcon name="edit" />
                                "Change release"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| edit(AlbumEdit { monitored: Some(!on), ..Default::default() })>
                                <LibIcon name=if on { "bookmark-off" } else { "bookmark" } />
                                {if on { "Unmonitor" } else { "Monitor" }}
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

    let track_list = move || {
        let monitored = album.get().and_then(Result::ok).is_some_and(|a| a.monitored);
        let list = match tracks.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| tracks.refetch()) /> }
                    .into_any();
            }
            Some(Ok(t)) => t,
        };
        if list.is_empty() {
            return view! {
                <EmptyState icon="music" title="No tracklist yet" description="The releases of this album have not been loaded from MusicBrainz yet; refresh the artist." compact=true />
            }
            .into_any();
        }
        let multi = list.iter().any(|t| t.disc_number > 1);
        let total: u64 = list.iter().filter_map(|t| t.duration_ms).sum();
        view! {
            <div class="table-wrap">
                <table class="table">
                    <caption class="visually-hidden">"Tracks"</caption>
                    <thead>
                        <tr>
                            <th scope="col">"#"</th>
                            <th scope="col">"Title"</th>
                            <th scope="col">"Duration"</th>
                            <th scope="col">"File"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {list.into_iter().map(|t| {
                            let (tone, label) = track_status(&t, monitored);
                            let num = if multi { format!("{}-{:02}", t.disc_number, t.track_number) } else { t.track_number.to_string() };
                            view! {
                                <tr>
                                    <td class="mono">{num}</td>
                                    <td class="cell-strong">
                                        {t.title.clone()}
                                        {t.artist_credit.clone().map(|c| view! { <span class="cell-sub">{c}</span> })}
                                    </td>
                                    <td class="mono">{duration(t.duration_ms)}</td>
                                    <td>
                                        <StatusBadge tone=tone label=label />
                                        {t.track_file.as_ref().map(|f| view! { <span class="cell-sub mono">{f.relative_path.clone()}</span> })}
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <p class="lib-count">{format!("Total length {}", duration(Some(total)))}</p>
        }
        .into_any()
    };

    let file_list = move || {
        let list = match files.get() {
            None => return view! { <Skeleton lines=2 /> }.into_any(),
            Some(Err(e)) => return view! { <ErrorBox error=e /> }.into_any(),
            Some(Ok(f)) => f,
        };
        if list.is_empty() {
            return view! { <EmptyState icon="folder" title="No files" description="Nothing of this album is on disk yet." compact=true /> }.into_any();
        }
        view! {
            <div class="table-wrap">
                <table class="table">
                    <caption class="visually-hidden">"Track files"</caption>
                    <thead>
                        <tr>
                            <th scope="col">"Path"</th>
                            <th scope="col">"Quality"</th>
                            <th scope="col">"Audio"</th>
                            <th scope="col">"Size"</th>
                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        {list.into_iter().map(|f| {
                            let audio = [
                                f.bitrate_kbps.map(|b| format!("{b} kbps")),
                                f.sample_rate_hz.map(|s| format!("{:.1} kHz", f64::from(s) / 1000.0)),
                                f.bit_depth.map(|b| format!("{b} bit")),
                                f.channels.map(|c| if c == 2 { "stereo".to_string() } else { format!("{c} ch") }),
                            ].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                            let f2 = f.clone();
                            view! {
                                <tr>
                                    <td class="mono">{f.relative_path.clone()}</td>
                                    <td><StatusBadge tone=if f.cutoff_met { crate::components::Tone::Success } else { crate::components::Tone::Warning } label=f.quality_name.clone() /></td>
                                    <td>{audio}</td>
                                    <td>{bytes(f.size)}</td>
                                    <td class="cell-actions">
                                        <button type="button" class="btn btn-sm btn-ghost btn-danger-text" aria-label=format!("Delete {}", f.relative_path)
                                            on:click=move |_| { file_to_delete.set(Some(f2.clone())); file_delete_open.set(true); }>
                                            <Icon name="trash" />
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
        {header}
        <section class="card">
            <header class="card-header"><h2 class="card-title">"Tracks"</h2></header>
            <div class="card-body">{track_list}</div>
        </section>
        <section class="card">
            <header class="card-header"><h2 class="card-title">"Files"</h2></header>
            <div class="card-body">{file_list}</div>
        </section>
        <ChooseReleaseModal open=release_open album=album_sig on_saved=Callback::new(move |()| refetch()) />
        <InteractiveSearchModal open=isearch_open target=isearch_target />
        <DeleteMediaDialog open=delete_open title="Delete album?"
            message=Signal::derive(move || album.get().and_then(Result::ok).map(|a| format!("“{}” will be removed from the library.", a.title)).unwrap_or_default())
            on_confirm=delete />
        <ConfirmDialog open=file_delete_open title="Delete file?"
            message=Signal::derive(move || file_to_delete.get().map(|f| format!("{} is deleted (to the recycle bin when one is configured).", f.relative_path)).unwrap_or_default())
            confirm_label="Delete" on_confirm=delete_file />
    }
}
