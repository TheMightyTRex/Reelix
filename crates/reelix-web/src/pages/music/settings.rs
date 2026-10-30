//! Music settings cards, shown on the general settings pages: naming templates
//! ([`MusicNamingCard`], on Naming) and tag / cover writing plus release preferences
//! ([`MusicMetadataCard`], on Metadata).

use leptos::ev;
use leptos::prelude::*;
use reelix_api_types::music::{MusicNamingDto, MusicSettingsDto};

use crate::components::Card;
use crate::components::settings_draft::{SettingsDraft, loaded_view, save_button};

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

const PATH: &str = "/settings/music";

/// Artist / album / track naming templates with a token cheat-sheet.
#[component]
pub fn MusicNamingCard() -> impl IntoView {
    let s = SettingsDraft::<MusicSettingsDto>::new(PATH);
    let focused = RwSignal::new("track_file");

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
        s.save("Music naming", |fresh, d| fresh.naming = d.naming.clone());
    };

    let form = move || {
        view! {
            <form class="form" on:submit=save>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.naming.rename)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.naming.rename = v); } />
                    "Rename track files on import"
                </label>
                {FIELDS.iter().map(|&(id, label, hint, get, set)| view! {
                    <div class="field">
                        <label for=format!("ms-{id}")>{label}</label>
                        <input id=format!("ms-{id}") class="input mono" type="text" spellcheck="false"
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
            <Card title="Music" subtitle="Artist, album and track files and folders.">{loaded_view(s, 6, form)}</Card>
            <Card title="Music tokens" subtitle="Click a token to add it to the last music field you edited.">
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

/// Tag and cover art writing for imported music, and which MusicBrainz release is preferred.
#[component]
pub fn MusicMetadataCard() -> impl IntoView {
    let s = SettingsDraft::<MusicSettingsDto>::new(PATH);
    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        s.save("Music metadata settings", |fresh, d| {
            fresh.write_tags = d.write_tags;
            fresh.embed_cover = d.embed_cover;
            fresh.save_cover_file = d.save_cover_file;
            fresh.preferred_release_countries = d.preferred_release_countries.clone();
            fresh.preferred_formats = d.preferred_formats.clone();
        });
    };
    let form = move || {
        view! {
            <form class="form" on:submit=save>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.write_tags)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.write_tags = v); } />
                    "Write tags (MusicBrainz ids, artist, album, track/disc numbers, date) into imported files"
                </label>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.embed_cover)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.embed_cover = v); } />
                    "Embed the cover from the Cover Art Archive"
                </label>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.save_cover_file)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.save_cover_file = v); } />
                    "Save cover.jpg into the album folder"
                </label>
                <p class="field-hint">
                    "Reelix only writes into files it owns: when tags are written, imports copy instead of hardlinking, so files still seeding are never modified. This uses extra disk space for torrents that keep seeding."
                </p>
                <h3 class="card-title">"Release preferences"</h3>
                <div class="field">
                    <label for="ms-countries">"Preferred release countries"</label>
                    <input id="ms-countries" class="input" type="text" placeholder="e.g. US, GB, XW"
                        prop:value=move || s.get(|d| list_text(&d.preferred_release_countries))
                        on:change=move |e| { let v = parse_list(&event_target_value(&e)); s.edit(|d| d.preferred_release_countries = v); } />
                    <span class="field-hint">"ISO country codes, comma separated (XW = worldwide). Picks which MusicBrainz release of an album is its default, and breaks ties when matching downloads."</span>
                </div>
                <div class="field">
                    <label for="ms-formats">"Preferred formats"</label>
                    <input id="ms-formats" class="input" type="text" placeholder="e.g. Digital Media, CD"
                        prop:value=move || s.get(|d| list_text(&d.preferred_formats))
                        on:change=move |e| { let v = parse_list(&event_target_value(&e)); s.edit(|d| d.preferred_formats = v); } />
                </div>
                {save_button(s.saving)}
            </form>
        }
    };
    view! {
        <Card title="Music" subtitle="Tags and cover art from MusicBrainz written into imported music, and which release of an album is preferred.">{loaded_view(s, 4, form)}</Card>
    }
}
