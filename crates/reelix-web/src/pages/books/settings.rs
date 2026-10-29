//! `/settings/books` — book root folders, naming templates, metadata / cover writing and
//! Calibre.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::books::{BookNamingDto, BookSettingsDto};
use reelix_api_types::content::{ContentKind, ContentRootFolderDto, CreateContentRootFolderRequest};

use crate::api;
use crate::components::books::kind_label;
use crate::components::library_common::bytes;
use crate::components::{Card, ConfirmDialog, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, Spinner};
use crate::pages::system::require_admin;
use crate::state::Toasts;

const TOKENS: &[(&str, &[&str])] = &[
    ("Author", &["{Author Name}", "{Author SortName}"]),
    (
        "Book",
        &[
            "{Book Title}",
            "{Book CleanTitle}",
            "{Book SeriesTitle}",
            "{Book SeriesPosition}",
            "{Release Year}",
            "{Edition Language}",
        ],
    ),
    (
        "File",
        &["{Quality Title}", "{Narrator}", "{Part:00}", "{PartCount}"],
    ),
];

type Getter = fn(&BookNamingDto) -> String;
type Setter = fn(&mut BookNamingDto, String);

const FIELDS: &[(&str, &str, &str, Getter, Setter)] = &[
    (
        "author_folder",
        "Author folder",
        "Folder under the root folder, shared by every book of the author.",
        |n| n.author_folder.clone(),
        |n, v| n.author_folder = v,
    ),
    (
        "ebook_file",
        "Ebook file",
        "File name without the extension.",
        |n| n.ebook_file.clone(),
        |n, v| n.ebook_file = v,
    ),
    (
        "audiobook_folder",
        "Audiobook folder",
        "Folder of one audiobook inside the author folder.",
        |n| n.audiobook_folder.clone(),
        |n, v| n.audiobook_folder = v,
    ),
    (
        "audiobook_file",
        "Audiobook file (single file)",
        "Used when the audiobook is one file, e.g. an .m4b.",
        |n| n.audiobook_file.clone(),
        |n, v| n.audiobook_file = v,
    ),
    (
        "audiobook_part_file",
        "Audiobook part (multi-file)",
        "Used for each part of a multi-file audiobook; include {Part:00}.",
        |n| n.audiobook_part_file.clone(),
        |n, v| n.audiobook_part_file = v,
    ),
];

#[component]
pub fn BookSettingsPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Books" subtitle="Root folders, file naming and the metadata Reelix writes into imported ebooks and audiobooks." />
        <RootFoldersCard />
        <SettingsForm />
    }
}

#[component]
fn RootFoldersCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let roots = LocalResource::new(|| api::get::<Vec<ContentRootFolderDto>>("/bookrootfolders"));
    let path = RwSignal::new(String::new());
    let kind = RwSignal::new(ContentKind::Ebook);
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
            kind: kind.get_untracked(),
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, ContentRootFolderDto>("/bookrootfolders", &req).await {
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
            match api::delete(&format!("/bookrootfolders/{}", r.id)).await {
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
            <EmptyState icon="folder" title="No book root folders" description="Add a folder for ebooks and one for audiobooks (they may not overlap with movie or series folders)." compact=true />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <div class="table-wrap">
                <table class="table">
                    <caption class="visually-hidden">"Book root folders"</caption>
                    <thead>
                        <tr>
                            <th scope="col">"Path"</th>
                            <th scope="col">"Format"</th>
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
                                    <td>{kind_label(r.kind)}</td>
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
        <Card title="Root folders" subtitle="Ebooks and audiobooks each live under their own root folders.">
            {list}
            <form class="lib-toolbar" on:submit=add>
                <input class="input grow mono" type="text" placeholder="/data/books" aria-label="Root folder path"
                    prop:value=move || path.get() on:input=move |e| path.set(event_target_value(&e)) />
                <select class="input" aria-label="Format" on:change=move |e| {
                    kind.set(if event_target_value(&e) == "audiobook" { ContentKind::Audiobook } else { ContentKind::Ebook })
                }>
                    <option value="ebook" selected=move || kind.get() == ContentKind::Ebook>"Ebooks"</option>
                    <option value="audiobook" selected=move || kind.get() == ContentKind::Audiobook>"Audiobooks"</option>
                </select>
                <button type="submit" class="btn btn-secondary" disabled=move || busy.get()>
                    <Icon name="plus" />
                    "Add root folder"
                </button>
            </form>
        </Card>
        <ConfirmDialog open=confirm_open title="Remove root folder?"
            message=Signal::derive(move || to_delete.get().map(|r| format!("{} is removed from Reelix; files on disk stay. Authors still using it block the removal.", r.path)).unwrap_or_default())
            confirm_label="Remove" on_confirm=delete />
    }
}

#[component]
fn SettingsForm() -> impl IntoView {
    let toasts = Toasts::expect();
    let loaded = LocalResource::new(|| api::get::<BookSettingsDto>("/settings/books"));
    let draft = RwSignal::new(None::<BookSettingsDto>);
    let saving = RwSignal::new(false);
    let focused = RwSignal::new("ebook_file");

    Effect::new(move |_| {
        if let Some(Ok(v)) = loaded.get() {
            draft.set(Some(v));
        }
    });

    let edit = move |f: &dyn Fn(&mut BookSettingsDto)| {
        draft.update(|d| {
            if let Some(d) = d.as_mut() {
                f(d)
            }
        })
    };
    let flag = move |get: fn(&BookSettingsDto) -> bool| draft.with(|d| d.as_ref().is_some_and(get));
    let text =
        move |get: fn(&BookSettingsDto) -> String| draft.with(|d| d.as_ref().map(get).unwrap_or_default());

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
            match api::put::<_, BookSettingsDto>("/settings/books", &d).await {
                Ok(s) => {
                    toasts.success("Book settings saved.");
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
                    "Rename book files on import"
                </label>
                {FIELDS.iter().map(|&(id, label, hint, get, set)| view! {
                    <div class="field">
                        <label for=format!("bs-{id}")>{label}</label>
                        <input id=format!("bs-{id}") class="input mono" type="text" spellcheck="false"
                            prop:value=move || draft.with(|d| d.as_ref().map(|d| get(&d.naming)).unwrap_or_default())
                            on:focus=move |_| focused.set(id)
                            on:input=move |e| { let v = event_target_value(&e); edit(&|d| set(&mut d.naming, v.clone())); } />
                        <span class="field-hint">{hint}</span>
                    </div>
                }).collect_view()}
                <p class="field-hint">"Colon replacement and illegal-character handling follow the general Naming settings."</p>

                <h3 class="card-title">"Metadata"</h3>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || flag(|d| d.write_metadata)
                        on:change=move |e| { let v = event_target_checked(&e); edit(&|d| d.write_metadata = v); } />
                    "Write metadata into imported files (EPUB OPF, audiobook tags)"
                </label>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || flag(|d| d.write_covers)
                        on:change=move |e| { let v = event_target_checked(&e); edit(&|d| d.write_covers = v); } />
                    "Embed cover art"
                </label>
                <p class="field-hint">
                    "Reelix only writes into files it owns: when metadata or covers are written, imports copy instead of hardlinking, so files still seeding are never modified. This uses extra disk space for torrents that keep seeding."
                </p>
                <div class="field">
                    <label for="bs-lang">"Metadata language"</label>
                    <input id="bs-lang" class="input" type="text" placeholder="e.g. en (empty = any)"
                        prop:value=move || text(|d| d.metadata_language.clone().unwrap_or_default())
                        on:input=move |e| { let v = event_target_value(&e); edit(&|d| d.metadata_language = Some(v.trim().to_string()).filter(|s| !s.is_empty())); } />
                </div>

                <h3 class="card-title">"Calibre"</h3>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || flag(|d| d.calibre.enabled)
                        on:change=move |e| { let v = event_target_checked(&e); edit(&|d| d.calibre.enabled = v); } />
                    "Also add imported ebooks to a Calibre library (calibredb add)"
                </label>
                <div class="field-row">
                    <div class="field">
                        <label for="bs-calibre-lib">"Calibre library path"</label>
                        <input id="bs-calibre-lib" class="input mono" type="text" placeholder="/data/calibre"
                            prop:value=move || text(|d| d.calibre.library_path.clone().unwrap_or_default())
                            on:input=move |e| { let v = event_target_value(&e); edit(&|d| d.calibre.library_path = Some(v.trim().to_string()).filter(|s| !s.is_empty())); } />
                    </div>
                    <div class="field">
                        <label for="bs-calibredb">"calibredb binary"</label>
                        <input id="bs-calibredb" class="input mono" type="text"
                            prop:value=move || text(|d| d.calibre.calibredb_path.clone())
                            on:input=move |e| { let v = event_target_value(&e); edit(&|d| d.calibre.calibredb_path = v.clone()); } />
                    </div>
                </div>
                <p class="field-hint">"Calibre failures only log a warning; the import itself still succeeds."</p>

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
                    <p class="field-hint">"Text inside braces is only kept when its token has a value, e.g. {Book Title}{ - Book SeriesTitle}{ Book SeriesPosition}."</p>
                </div>
            </Card>
        </div>
    }
}
