//! Music dialogs: add artist, add album, edit artist, choose release and rename preview.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::library::RenamePreviewDto;
use reelix_api_types::music::{
    AddAlbumRequest, AddArtistRequest, AlbumDto, AlbumLookupDto, ArtistDto, ArtistEdit, ArtistLookupDto,
    ArtistMonitor, ChooseReleaseRequest, NewAlbumMonitor,
};

use super::{
    ARTIST_MONITOR, IdSelect, MusicOptions, NEW_ALBUM_MONITOR, album_type_label, metadata_items,
    profile_items, root_items, status_label,
};
use crate::api;
use crate::components::library_common::{pref_get, pref_set, toggle_id};
use crate::components::library_forms::EnumSelect;
use crate::components::poster::Poster;
use crate::components::{EmptyState, ErrorBox, Icon, Modal, Skeleton, Spinner};
use crate::state::Toasts;

fn busy_icon(busy: RwSignal<bool>, icon: &'static str) -> impl IntoView {
    move || {
        if busy.get() {
            view! { <Spinner /> }.into_any()
        } else {
            view! { <Icon name=icon /> }.into_any()
        }
    }
}

fn remembered(key: &str) -> RwSignal<i64> {
    RwSignal::new(
        pref_get(&format!("music.add.{key}"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    )
}

/// Toggleable tag chips.
#[component]
pub fn MusicTagPicker(opts: MusicOptions, value: RwSignal<Vec<i64>>) -> impl IntoView {
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

/// Root folder / quality profile / metadata profile fields.
#[component]
fn ProfileFields(
    opts: MusicOptions,
    root: RwSignal<i64>,
    profile: RwSignal<i64>,
    metadata: RwSignal<i64>,
    #[prop(into)] prefix: String,
) -> impl IntoView {
    let (rid, pid, mid) = (
        format!("{prefix}-root"),
        format!("{prefix}-profile"),
        format!("{prefix}-meta"),
    );
    view! {
        <div class="field">
            <label for=rid.clone()>"Root folder"</label>
            <IdSelect items=root_items(opts) value=root id=rid />
            {move || opts.root_list().is_empty().then(|| view! {
                <p class="field-hint text-danger">"No music root folders yet. " <A href="/settings/music">"Add one in Music settings"</A></p>
            })}
        </div>
        <div class="field-row">
            <div class="field">
                <label for=pid.clone()>"Quality profile"</label>
                <IdSelect items=profile_items(opts) value=profile id=pid />
                {move || opts.profile_list().is_empty().then(|| view! {
                    <p class="field-hint text-danger">"No music quality profiles. " <A href="/settings/profiles">"Create one under Profiles → Music"</A></p>
                })}
            </div>
            <div class="field">
                <label for=mid.clone()>"Metadata profile"</label>
                <IdSelect items=metadata_items(opts) value=metadata id=mid />
                <span class="field-hint">"Which album types (albums, EPs, singles, live…) are added."</span>
            </div>
        </div>
    }
}

fn problem(root: i64, profile: i64, metadata: i64) -> Option<&'static str> {
    if root == 0 {
        Some("Choose a music root folder (add one under Settings → Music).")
    } else if profile == 0 {
        Some("Choose a music quality profile.")
    } else if metadata == 0 {
        Some("Choose a metadata profile.")
    } else {
        None
    }
}

/// Adds an artist from a MusicBrainz lookup result.
#[component]
pub fn AddArtistModal(
    open: RwSignal<bool>,
    target: RwSignal<Option<ArtistLookupDto>>,
    opts: MusicOptions,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let root = remembered("root");
    let profile = remembered("profile");
    let metadata = remembered("metadata");
    let monitor = RwSignal::new(ArtistMonitor::All);
    let new_items = RwSignal::new(NewAlbumMonitor::All);
    let tags = RwSignal::new(Vec::<i64>::new());
    let search = RwSignal::new(pref_get("music.add.search").is_none_or(|v| v == "1"));
    let busy = RwSignal::new(false);

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(t) = target.get_untracked() else { return };
        let (r, p, m) = (
            root.get_untracked(),
            profile.get_untracked(),
            metadata.get_untracked(),
        );
        if let Some(msg) = problem(r, p, m) {
            toasts.error(msg);
            return;
        }
        pref_set("music.add.root", &r.to_string());
        pref_set("music.add.profile", &p.to_string());
        pref_set("music.add.metadata", &m.to_string());
        pref_set("music.add.search", if search.get_untracked() { "1" } else { "0" });
        let req = AddArtistRequest {
            musicbrainz_id: t.musicbrainz_id.clone(),
            quality_profile_id: p,
            metadata_profile_id: m,
            root_folder_id: r,
            monitored: monitor.get_untracked() != ArtistMonitor::None,
            monitor: monitor.get_untracked(),
            monitor_new_items: new_items.get_untracked(),
            selected_release_group_ids: Vec::new(),
            tags: tags.get_untracked(),
            search_on_add: search.get_untracked(),
        };
        busy.set(true);
        let navigate = navigate.clone();
        spawn_local(async move {
            let res = api::post::<_, ArtistDto>("/artists", &req).await;
            busy.set(false);
            match res {
                Ok(a) => {
                    toasts.success(format!("Added “{}”.", a.name));
                    open.set(false);
                    navigate(&format!("/music/artists/{}", a.id), Default::default());
                }
                Err(e) => toasts.error(format!("Could not add: {e}")),
            }
        });
    };

    view! {
        <Modal open=open title="Add artist">
            <form class="form" on:submit=submit.clone()>
                {move || target.get().map(|t| view! {
                    <div class="add-head">
                        <Poster title=t.name.clone() url=t.image.clone() />
                        <div class="overview-body">
                            <span class="overview-title">{t.name.clone()}</span>
                            <div class="meta-line">
                                {t.artist_type.clone().map(|v| view! { <span>{v}</span> })}
                                {t.country.clone().map(|v| view! { <span>{v}</span> })}
                                {t.disambiguation.clone().map(|v| view! { <span>{v}</span> })}
                            </div>
                        </div>
                    </div>
                })}
                <ProfileFields opts=opts root=root profile=profile metadata=metadata prefix="ma" />
                <div class="field-row">
                    <div class="field">
                        <label for="ma-monitor">"Monitor"</label>
                        <EnumSelect table=&ARTIST_MONITOR value=monitor id="ma-monitor" />
                    </div>
                    <div class="field">
                        <label for="ma-new">"New albums"</label>
                        <EnumSelect table=&NEW_ALBUM_MONITOR value=new_items id="ma-new" />
                    </div>
                </div>
                <div class="field">
                    <span class="field-label">"Tags"</span>
                    <MusicTagPicker opts=opts value=tags />
                </div>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || search.get() on:change=move |e| search.set(event_target_checked(&e)) />
                    "Start searching for missing albums"
                </label>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {busy_icon(busy, "plus")}
                        "Add artist"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Adds one album (and its artist, with nothing else monitored, when needed).
#[component]
pub fn AddAlbumModal(
    open: RwSignal<bool>,
    target: RwSignal<Option<AlbumLookupDto>>,
    opts: MusicOptions,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let root = remembered("root");
    let profile = remembered("profile");
    let metadata = remembered("metadata");
    let search = RwSignal::new(pref_get("music.add.search").is_none_or(|v| v == "1"));
    let busy = RwSignal::new(false);

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(t) = target.get_untracked() else { return };
        let (r, p, m) = (
            root.get_untracked(),
            profile.get_untracked(),
            metadata.get_untracked(),
        );
        if let Some(msg) = problem(r, p, m) {
            toasts.error(msg);
            return;
        }
        let req = AddAlbumRequest {
            musicbrainz_release_group_id: t.musicbrainz_release_group_id.clone(),
            quality_profile_id: Some(p),
            metadata_profile_id: Some(m),
            root_folder_id: Some(r),
            tags: Vec::new(),
            search_on_add: search.get_untracked(),
            added_by_request_id: None,
        };
        busy.set(true);
        let navigate = navigate.clone();
        spawn_local(async move {
            let res = api::post::<_, AlbumDto>("/albums", &req).await;
            busy.set(false);
            match res {
                Ok(a) => {
                    toasts.success(format!("Added “{}”.", a.title));
                    open.set(false);
                    navigate(&format!("/music/albums/{}", a.id), Default::default());
                }
                Err(e) => toasts.error(format!("Could not add: {e}")),
            }
        });
    };

    view! {
        <Modal open=open title="Add album">
            <form class="form" on:submit=submit.clone()>
                {move || target.get().map(|t| view! {
                    <div class="add-head">
                        <Poster title=t.title.clone() url=t.cover.clone() />
                        <div class="overview-body">
                            <span class="overview-title">{t.title.clone()}{t.year.map(|y| format!(" ({y})"))}</span>
                            <div class="meta-line">
                                <span>{format!("by {}", t.artist_name)}</span>
                                <span>{album_type_label(t.album_type)}</span>
                            </div>
                        </div>
                    </div>
                })}
                <p class="field-hint">"When the artist is not in the library yet it is added too, with only this album monitored."</p>
                <ProfileFields opts=opts root=root profile=profile metadata=metadata prefix="mb" />
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || search.get() on:change=move |e| search.set(event_target_checked(&e)) />
                    "Start searching for this album"
                </label>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {busy_icon(busy, "plus")}
                        "Add album"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Edits an artist's monitoring, profiles, folder and tags.
#[component]
pub fn ArtistEditModal(
    open: RwSignal<bool>,
    #[prop(into)] artist: Signal<Option<ArtistDto>>,
    opts: MusicOptions,
    on_saved: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let monitored = RwSignal::new(true);
    let new_items = RwSignal::new(NewAlbumMonitor::All);
    let profile = RwSignal::new(0i64);
    let metadata = RwSignal::new(0i64);
    let root = RwSignal::new(0i64);
    let path = RwSignal::new(String::new());
    let move_files = RwSignal::new(false);
    let tags = RwSignal::new(Vec::<i64>::new());
    let busy = RwSignal::new(false);

    Effect::new(move |_| {
        if open.get()
            && let Some(a) = artist.get_untracked()
        {
            monitored.set(a.monitored);
            new_items.set(a.monitor_new_items);
            profile.set(a.quality_profile_id);
            metadata.set(a.metadata_profile_id);
            root.set(a.root_folder_id);
            path.set(a.path.clone());
            move_files.set(false);
            tags.set(a.tags.clone());
        }
    });

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(a) = artist.get_untracked() else { return };
        let new_path = path.get_untracked().trim().to_string();
        let edit = ArtistEdit {
            monitored: Some(monitored.get_untracked()),
            monitor_new_items: Some(new_items.get_untracked()),
            quality_profile_id: Some(profile.get_untracked()),
            metadata_profile_id: Some(metadata.get_untracked()),
            root_folder_id: (root.get_untracked() != a.root_folder_id).then(|| root.get_untracked()),
            path: (new_path != a.path && !new_path.is_empty()).then_some(new_path),
            tags: Some(tags.get_untracked()),
            move_files: move_files.get_untracked(),
        };
        busy.set(true);
        spawn_local(async move {
            let res = api::put::<_, ArtistDto>(&format!("/artists/{}", a.id), &edit).await;
            busy.set(false);
            match res {
                Ok(_) => {
                    toasts.success("Artist saved.");
                    open.set(false);
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };

    view! {
        <Modal open=open title="Edit artist">
            <form class="form" on:submit=submit>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || monitored.get() on:change=move |e| monitored.set(event_target_checked(&e)) />
                    "Monitored"
                </label>
                <div class="field">
                    <label for="me-new">"New albums"</label>
                    <EnumSelect table=&NEW_ALBUM_MONITOR value=new_items id="me-new" />
                </div>
                <ProfileFields opts=opts root=root profile=profile metadata=metadata prefix="me" />
                <div class="field">
                    <label for="me-path">"Path"</label>
                    <input id="me-path" class="input mono" type="text" prop:value=move || path.get()
                        on:input=move |e| path.set(event_target_value(&e)) />
                </div>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || move_files.get() on:change=move |e| move_files.set(event_target_checked(&e)) />
                    "Move the files when the path changes"
                </label>
                <div class="field">
                    <span class="field-label">"Tags"</span>
                    <MusicTagPicker opts=opts value=tags />
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {busy_icon(busy, "check")}
                        "Save"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Lists the releases (editions) of an album and makes the chosen one the wanted release.
#[component]
pub fn ChooseReleaseModal(
    open: RwSignal<bool>,
    #[prop(into)] album: Signal<Option<AlbumDto>>,
    on_saved: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let busy = RwSignal::new(None::<i64>);
    let choose = move |album_id: i64, release_id: i64| {
        busy.set(Some(release_id));
        spawn_local(async move {
            let res = api::put::<_, AlbumDto>(
                &format!("/albums/{album_id}/release"),
                &ChooseReleaseRequest { release_id },
            )
            .await;
            busy.set(None);
            match res {
                Ok(_) => {
                    toasts.success("Release changed.");
                    open.set(false);
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not change the release: {e}")),
            }
        });
    };
    view! {
        <Modal open=open title="Choose release">
            <p class="modal-text">"The selected release's tracklist is what Reelix searches for and matches downloads against. Existing files are kept and re-linked by disc and track number."</p>
            {move || {
                let Some(a) = album.get() else { return view! { <Skeleton lines=3 /> }.into_any() };
                if a.releases.is_empty() {
                    return view! { <EmptyState icon="music" title="No releases loaded" description="Refresh the artist to load the releases from MusicBrainz." compact=true /> }.into_any();
                }
                let album_id = a.id;
                view! {
                    <div class="table-wrap">
                        <table class="table">
                            <caption class="visually-hidden">"Releases"</caption>
                            <thead>
                                <tr>
                                    <th scope="col">"Title"</th>
                                    <th scope="col">"Format"</th>
                                    <th scope="col">"Tracks"</th>
                                    <th scope="col">"Country"</th>
                                    <th scope="col">"Date"</th>
                                    <th scope="col">"Label"</th>
                                    <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                                </tr>
                            </thead>
                            <tbody>
                                {a.releases.into_iter().map(|r| {
                                    let rid = r.id;
                                    view! {
                                        <tr class:row-selected=r.selected>
                                            <td class="cell-strong">
                                                {r.title.clone()}
                                                {r.disambiguation.clone().map(|d| view! { <span class="cell-sub">{d}</span> })}
                                                {r.status.filter(|s| *s != reelix_api_types::music::ReleaseStatus::Official)
                                                    .map(|s| view! { <span class="cell-sub">{status_label(s)}</span> })}
                                            </td>
                                            <td>{r.format.clone().unwrap_or_else(|| "—".into())}</td>
                                            <td>{if r.disc_count > 1 { format!("{} ({} discs)", r.track_count, r.disc_count) } else { r.track_count.to_string() }}</td>
                                            <td>{r.country.clone().unwrap_or_else(|| "—".into())}</td>
                                            <td>{r.release_date.map(|d| d.to_string()).unwrap_or_else(|| "—".into())}</td>
                                            <td>{r.label.clone().unwrap_or_default()}{r.catalog_number.clone().map(|c| view! { <span class="cell-sub mono">{c}</span> })}</td>
                                            <td class="cell-actions">
                                                {if r.selected {
                                                    view! { <span class="badge tone-success">"Selected"</span> }.into_any()
                                                } else {
                                                    view! {
                                                        <button type="button" class="btn btn-sm btn-secondary" disabled=move || busy.get().is_some()
                                                            on:click=move |_| choose(album_id, rid)>
                                                            {move || if busy.get() == Some(rid) { view! { <Spinner /> }.into_any() } else { view! { <Icon name="check" /> }.into_any() }}
                                                            "Select"
                                                        </button>
                                                    }.into_any()
                                                }}
                                            </td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                }.into_any()
            }}
            <div class="modal-footer">
                <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Close"</button>
            </div>
        </Modal>
    }
}

/// Lists track files whose names differ from the music naming templates; renames on confirm.
#[component]
pub fn MusicRenameModal(open: RwSignal<bool>, artist_id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let preview = LocalResource::new(move || {
        let o = open.get();
        async move {
            if o {
                Some(api::get::<Vec<RenamePreviewDto>>(&format!("/music/rename?artist_id={artist_id}")).await)
            } else {
                None
            }
        }
    });
    let busy = RwSignal::new(false);
    let run = move |_| {
        busy.set(true);
        spawn_local(async move {
            match api::post_empty::<EnqueuedJob>(&format!("/music/rename?artist_id={artist_id}")).await {
                Ok(_) => {
                    toasts.success("Rename queued.");
                    open.set(false);
                }
                Err(e) => toasts.error(format!("Could not start the rename: {e}")),
            }
            busy.set(false);
        });
    };
    view! {
        <Modal open=open title="Rename files">
            {move || match preview.get() {
                None | Some(None) => view! { <Skeleton lines=3 /> }.into_any(),
                Some(Some(Err(e))) => view! { <ErrorBox error=e /> }.into_any(),
                Some(Some(Ok(list))) if list.is_empty() => view! {
                    <EmptyState icon="check" title="Nothing to rename" description="Every file already matches the naming settings." compact=true />
                }.into_any(),
                Some(Some(Ok(list))) => view! {
                    <div class="table-wrap">
                        <table class="table">
                            <caption class="visually-hidden">"Planned renames"</caption>
                            <thead><tr><th scope="col">"Current"</th><th scope="col">"New"</th></tr></thead>
                            <tbody>
                                {list.into_iter().map(|p| view! {
                                    <tr><td class="mono">{p.existing_path}</td><td class="mono">{p.new_path}</td></tr>
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                }.into_any(),
            }}
            <div class="modal-footer">
                <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                <button type="button" class="btn btn-primary" disabled=move || busy.get()
                    on:click=run>
                    {busy_icon(busy, "check")}
                    "Rename"
                </button>
            </div>
        </Modal>
    }
}
