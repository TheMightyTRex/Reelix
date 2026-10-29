//! `/settings/music` — music root folders, naming templates, tag / cover writing and release
//! preferences.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::content::{ContentKind, ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_api_types::music::{MusicNamingDto, MusicSettingsDto};

use crate::api;
use crate::components::library_common::bytes;
use crate::components::{Card, ConfirmDialog, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, Spinner};
use crate::pages::system::require_admin;
use crate::state::Toasts;

const TOKENS: &[(&str, &[&str])] = &[
    (
        "Artist",
        &["{Artist Name}", "{Artist SortName}", "{Artist CleanName}"],
    ),
    (
        "Album",
        &[
            "{Album Title}",
            "{Album CleanTitle}",
            "{Album Type}",
            "{Release Year}",
            "{Album Disambiguation}",
        ],
    ),
    ("Disc", &["{medium:00}", "{Medium Name}"]),
    ("Track", &["{track:00}", "{Track Title}", "{Track ArtistName}"]),
    (
        "File",
        &[
            "{Quality Title}",
            "{MediaInfo AudioCodec}",
            "{MediaInfo AudioBitRate}",
        ],
    ),
];

type Getter = fn(&MusicNamingDto) -> String;
type Setter = fn(&mut MusicNamingDto, String);

const FIELDS: &[(&str, &str, &str, Getter, Setter)] = &[
    (
        "artist_folder",
        "Artist folder",
        "Folder under the root folder.",
        |n| n.artist_folder.clone(),
        |n, v| n.artist_folder = v,
    ),
    (
        "album_folder",
        "Album folder",
        "Folder of one album inside the artist folder.",
        |n| n.album_folder.clone(),
        |n, v| n.album_folder = v,
    ),
    (
        "track_file",
        "Track file",
        "File name without the extension.",
        |n| n.track_file.clone(),
        |n, v| n.track_file = v,
    ),
    (
        "multi_disc_track_file",
        "Multi-disc track file",
        "Used for albums with several discs when there is no disc folder; include {medium:00}.",
        |n| n.multi_disc_track_file.clone(),
        |n, v| n.multi_disc_track_file = v,
    ),
    (
        "multi_disc_folder",
        "Disc folder (optional)",
        "e.g. CD {medium} — puts each disc of a multi-disc album into its own folder. Empty = no disc folders.",
        |n| n.multi_disc_folder.clone().unwrap_or_default(),
        |n, v| n.multi_disc_folder = Some(v).filter(|s| !s.trim().is_empty()),
    ),
];

fn list_text(v: &[String]) -> String {
    v.join(", ")
}

fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[component]
pub fn MusicSettingsPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Music" subtitle="Root folders, file naming, tags and cover art written into imported music, and release preferences." />
        <RootFoldersCard />
        <SettingsForm />
    }
}

#[component]
fn RootFoldersCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let roots = LocalResource::new(|| api::get::<Vec<ContentRootFolderDto>>("/musicrootfolders"));
    let path = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let to_delete = RwSignal::new(None::<ContentRootFolderDto>);
    let confirm_open = RwSignal::new(false);

    let add = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let p = path.get_untracked().trim().to_string();
        if p.is_empty() {
            return;
        }
        let req = CreateContentRootFolderRequest {
            path: p,
            kind: ContentKind::Music,
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, ContentRootFolderDto>("/musicrootfolders", &req).await {
                Ok(_) => {
                    toasts.success("Root folder added.");
                    path.set(String::new());
                    roots.refetch();
                }
                Err(e) => toasts.error(format!("Could not add the root folder: {e}")),
            }
            busy.set(false);
        });
    };
    let delete = Callback::new(move |()| {
        let Some(r) = to_delete.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/musicrootfolders/{}", r.id)).await {
                Ok(()) => {
                    toasts.success("Root folder removed.");
                    roots.refetch();
                }
                Err(e) => toasts.error(format!("Could not remove: {e}")),
            }
        });
    });

    let list = move || {
        match roots.get() {
        None => view! { <Skeleton lines=2 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| roots.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState icon="folder" title="No music root folders" description="Add the folder your music library lives in (it may not overlap with movie, series or book folders)." compact=true />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <div class="table-wrap">
                <table class="table">
                    <caption class="visually-hidden">"Music root folders"</caption>
                    <thead>
                        <tr>
                            <th scope="col">"Path"</th>
                            <th scope="col">"Free space"</th>
                            <th scope="col">"Unmapped folders"</th>
                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        {list.into_iter().map(|r| {
                            let r2 = r.clone();
                            view! {
                                <tr>
                                    <td class="mono">
                                        {r.path.clone()}
                                        {(!r.accessible).then(|| view! { <span class="cell-sub text-danger">"Not accessible"</span> })}
                                    </td>
                                    <td>{r.free_space.map(bytes).unwrap_or_else(|| "—".into())}</td>
                                    <td>{r.unmapped_folders.len()}</td>
                                    <td class="cell-actions">
                                        <button type="button" class="btn btn-sm btn-ghost btn-danger-text"
                                            aria-label=format!("Remove {}", r.path)
                                            on:click=move |_| { to_delete.set(Some(r2.clone())); confirm_open.set(true); }>
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
        .into_any(),
    }
    };

    view! {
        <Card title="Root folders" subtitle="Artists live in folders under these roots.">
            {list}
            <form class="lib-toolbar" on:submit=add>
                <input class="input grow mono" type="text" placeholder="/data/music" aria-label="Root folder path"
                    prop:value=move || path.get() on:input=move |e| path.set(event_target_value(&e)) />
                <button type="submit" class="btn btn-secondary" disabled=move || busy.get()>
                    <Icon name="plus" />
                    "Add root folder"
                </button>
            </form>
        </Card>
        <ConfirmDialog open=confirm_open title="Remove root folder?"
            message=Signal::derive(move || to_delete.get().map(|r| format!("{} is removed from Reelix; files on disk stay. Artists still using it block the removal.", r.path)).unwrap_or_default())
            confirm_label="Remove" on_confirm=delete />
    }
}

#[component]
fn SettingsForm() -> impl IntoView {
    let toasts = Toasts::expect();
    let loaded = LocalResource::new(|| api::get::<MusicSettingsDto>("/settings/music"));
    let draft = RwSignal::new(None::<MusicSettingsDto>);
    let saving = RwSignal::new(false);
    let focused = RwSignal::new("track_file");

    Effect::new(move |_| {
        if let Some(Ok(v)) = loaded.get() {
            draft.set(Some(v));
        }
    });

    let edit = move |f: &dyn Fn(&mut MusicSettingsDto)| {
        draft.update(|d| {
            if let Some(d) = d.as_mut() {
                f(d)
            }
        })
    };
    let flag = move |get: fn(&MusicSettingsDto) -> bool| draft.with(|d| d.as_ref().is_some_and(get));

    let insert_token = move |tok: &'static str| {
        let field = focused.get_untracked();
        if let Some((_, _, _, get, set)) = FIELDS.iter().find(|f| f.0 == field) {
            edit(&|d| {
                let mut v = get(&d.naming);
                if !v.is_empty() && !v.ends_with(' ') {
                    v.push(' ');
                }
                v.push_str(tok);
                set(&mut d.naming, v);
            });
        }
    };

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(d) = draft.get_untracked() else { return };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, MusicSettingsDto>("/settings/music", &d).await {
                Ok(s) => {
                    toasts.success("Music settings saved.");
                    draft.set(Some(s));
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    let form = move || {
        match loaded.get() {
        None => view! { <Skeleton lines=8 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| loaded.refetch()) /> }.into_any(),
        Some(Ok(_)) => view! {
            <form class="form" on:submit=save>
                <h3 class="card-title">"Naming"</h3>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || flag(|d| d.naming.rename)
                        on:change=move |e| { let v = event_target_checked(&e); edit(&|d| d.naming.rename = v); } />
                    "Rename track files on import"
                </label>
                {FIELDS.iter().map(|&(id, label, hint, get, set)| view! {
                    <div class="field">
                        <label for=format!("ms-{id}")>{label}</label>
                        <input id=format!("ms-{id}") class="input mono" type="text" spellcheck="false"
                            prop:value=move || draft.with(|d| d.as_ref().map(|d| get(&d.naming)).unwrap_or_default())
                            on:focus=move |_| focused.set(id)
                            on:input=move |e| { let v = event_target_value(&e); edit(&|d| set(&mut d.naming, v.clone())); } />
                        <span class="field-hint">{hint}</span>
                    </div>
                }).collect_view()}
                <p class="field-hint">"Colon replacement and illegal-character handling follow the general Naming settings."</p>

                <h3 class="card-title">"Tags and cover art"</h3>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || flag(|d| d.write_tags)
                        on:change=move |e| { let v = event_target_checked(&e); edit(&|d| d.write_tags = v); } />
                    "Write tags (MusicBrainz ids, artist, album, track/disc numbers, date) into imported files"
                </label>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || flag(|d| d.embed_cover)
                        on:change=move |e| { let v = event_target_checked(&e); edit(&|d| d.embed_cover = v); } />
                    "Embed the cover from the Cover Art Archive"
                </label>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || flag(|d| d.save_cover_file)
                        on:change=move |e| { let v = event_target_checked(&e); edit(&|d| d.save_cover_file = v); } />
                    "Save cover.jpg into the album folder"
                </label>
                <p class="field-hint">
                    "Reelix only writes into files it owns: when tags are written, imports copy instead of hardlinking, so files still seeding are never modified. This uses extra disk space for torrents that keep seeding."
                </p>

                <h3 class="card-title">"Release preferences"</h3>
                <div class="field">
                    <label for="ms-countries">"Preferred release countries"</label>
                    <input id="ms-countries" class="input" type="text" placeholder="e.g. US, GB, XW"
                        prop:value=move || draft.with(|d| d.as_ref().map(|d| list_text(&d.preferred_release_countries)).unwrap_or_default())
                        on:change=move |e| { let v = parse_list(&event_target_value(&e)); edit(&|d| d.preferred_release_countries = v.clone()); } />
                    <span class="field-hint">"ISO country codes, comma separated (XW = worldwide). Used to pick the default release and to break ties when matching downloads."</span>
                </div>
                <div class="field">
                    <label for="ms-formats">"Preferred formats"</label>
                    <input id="ms-formats" class="input" type="text" placeholder="e.g. Digital Media, CD"
                        prop:value=move || draft.with(|d| d.as_ref().map(|d| list_text(&d.preferred_formats)).unwrap_or_default())
                        on:change=move |e| { let v = parse_list(&event_target_value(&e)); edit(&|d| d.preferred_formats = v.clone()); } />
                </div>

                <div class="modal-footer">
                    <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                        {move || if saving.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="check" /> }.into_any() }}
                        "Save changes"
                    </button>
                </div>
            </form>
        }
        .into_any(),
    }
    };

    view! {
        <div class="split-grid naming-grid">
            <Card title="Settings">{form}</Card>
            <Card title="Tokens" subtitle="Click a token to add it to the last naming field you edited.">
                <div class="stack">
                    {TOKENS.iter().map(|(group, toks)| view! {
                        <div class="field">
                            <span class="field-label">{*group}</span>
                            <div class="token-grid">
                                {toks.iter().map(|t| {
                                    let t: &'static str = t;
                                    view! { <button type="button" class="token" on:click=move |_| insert_token(t)>{t}</button> }
                                }).collect_view()}
                            </div>
                        </div>
                    }).collect_view()}
                    <p class="field-hint">"Text inside braces is only kept when its token has a value, e.g. {Album Title}{ (Album Disambiguation)}."</p>
                </div>
            </Card>
        </div>
    }
}
