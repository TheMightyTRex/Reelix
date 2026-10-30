//! Book settings cards, shown on the general settings pages: naming templates
//! ([`BookNamingCard`], on Naming), metadata / cover writing ([`BookMetadataCard`], on Metadata)
//! and Calibre ([`BookCalibreCard`], on Media Management).

use leptos::ev;
use leptos::prelude::*;
use reelix_api_types::books::{BookNamingDto, BookSettingsDto};

use crate::components::Card;
use crate::components::settings_draft::{SettingsDraft, loaded_view, save_button};

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

const PATH: &str = "/settings/books";

/// Adding imported ebooks to a Calibre library (shown on Media Management).
#[component]
pub fn BookCalibreCard() -> impl IntoView {
    let s = SettingsDraft::<BookSettingsDto>::new(PATH);
    let text = move |get: fn(&BookSettingsDto) -> String| s.get(get);
    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        s.save("Calibre settings", |fresh, d| fresh.calibre = d.calibre.clone());
    };
    let form = move || {
        view! {
            <form class="form" on:submit=save>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.calibre.enabled)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.calibre.enabled = v); } />
                    "Also add imported ebooks to a Calibre library (calibredb add)"
                </label>
                <div class="field-row">
                    <div class="field">
                        <label for="bs-calibre-lib">"Calibre library path"</label>
                        <input id="bs-calibre-lib" class="input mono" type="text" placeholder="/data/calibre"
                            prop:value=move || text(|d| d.calibre.library_path.clone().unwrap_or_default())
                            on:input=move |e| { let v = event_target_value(&e); s.edit(|d| d.calibre.library_path = Some(v.trim().to_string()).filter(|s| !s.is_empty())); } />
                    </div>
                    <div class="field">
                        <label for="bs-calibredb">"calibredb binary"</label>
                        <input id="bs-calibredb" class="input mono" type="text"
                            prop:value=move || text(|d| d.calibre.calibredb_path.clone())
                            on:input=move |e| { let v = event_target_value(&e); s.edit(|d| d.calibre.calibredb_path = v); } />
                    </div>
                </div>
                <p class="field-hint">"Calibre failures only log a warning; the import itself still succeeds."</p>
                {save_button(s.saving)}
            </form>
        }
    };
    view! {
        <Card title="Calibre" subtitle="Also add imported ebooks to a Calibre library.">{loaded_view(s, 4, form)}</Card>
    }
}

/// Book file / folder naming templates with a token cheat-sheet.
#[component]
pub fn BookNamingCard() -> impl IntoView {
    let s = SettingsDraft::<BookSettingsDto>::new(PATH);
    let focused = RwSignal::new("ebook_file");

    let insert_token = move |tok: &'static str| {
        let field = focused.get_untracked();
        if let Some((_, _, _, get, set)) = FIELDS.iter().find(|f| f.0 == field) {
            s.edit(|d| {
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
        s.save("Book naming", |fresh, d| fresh.naming = d.naming.clone());
    };

    let form = move || {
        view! {
            <form class="form" on:submit=save>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.naming.rename)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.naming.rename = v); } />
                    "Rename book files on import"
                </label>
                {FIELDS.iter().map(|&(id, label, hint, get, set)| view! {
                    <div class="field">
                        <label for=format!("bs-{id}")>{label}</label>
                        <input id=format!("bs-{id}") class="input mono" type="text" spellcheck="false"
                            prop:value=move || s.get(|d| get(&d.naming))
                            on:focus=move |_| focused.set(id)
                            on:input=move |e| { let v = event_target_value(&e); s.edit(|d| set(&mut d.naming, v)); } />
                        <span class="field-hint">{hint}</span>
                    </div>
                }).collect_view()}
                <p class="field-hint">"Colon replacement and illegal-character handling follow the settings above."</p>
                {save_button(s.saving)}
            </form>
        }
    };

    view! {
        <div class="split-grid naming-grid">
            <Card title="Books" subtitle="Ebook and audiobook files and folders.">{loaded_view(s, 6, form)}</Card>
            <Card title="Book tokens" subtitle="Click a token to add it to the last book field you edited.">
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

/// Metadata / cover writing for imported ebooks and audiobooks, and the metadata language.
#[component]
pub fn BookMetadataCard() -> impl IntoView {
    let s = SettingsDraft::<BookSettingsDto>::new(PATH);
    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        s.save("Book metadata settings", |fresh, d| {
            fresh.write_metadata = d.write_metadata;
            fresh.write_covers = d.write_covers;
            fresh.metadata_language = d.metadata_language.clone();
        });
    };
    let form = move || {
        view! {
            <form class="form" on:submit=save>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.write_metadata)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.write_metadata = v); } />
                    "Write metadata into imported files (EPUB OPF, audiobook tags)"
                </label>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.write_covers)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.write_covers = v); } />
                    "Embed cover art"
                </label>
                <p class="field-hint">
                    "Reelix only writes into files it owns: when metadata or covers are written, imports copy instead of hardlinking, so files still seeding are never modified. This uses extra disk space for torrents that keep seeding."
                </p>
                <div class="field">
                    <label for="bs-lang">"Metadata language"</label>
                    <input id="bs-lang" class="input" type="text" placeholder="e.g. en (empty = any)"
                        prop:value=move || s.get(|d| d.metadata_language.clone().unwrap_or_default())
                        on:input=move |e| { let v = event_target_value(&e); s.edit(|d| d.metadata_language = Some(v.trim().to_string()).filter(|s| !s.is_empty())); } />
                </div>
                {save_button(s.saving)}
            </form>
        }
    };
    view! {
        <Card title="Books" subtitle="Metadata from Open Library, written into imported ebooks and audiobooks.">{loaded_view(s, 4, form)}</Card>
    }
}
