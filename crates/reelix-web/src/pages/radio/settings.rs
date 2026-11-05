//! `/settings/radio` — the Audiobookshelf podcast library, the default follow choice, the
//! sources and search preferences; radio root folders are managed in Media management. The
//! naming of filed episodes ([`RadioNamingCard`]) is shown on Settings → Naming.

use leptos::ev;
use leptos::prelude::*;
use leptos_router::components::A;
use reelix_api_types::content::ContentRootFolderDto;
use reelix_api_types::radio::{RadioNamingDto, RadioProviderDto, RadioSettingsDto};

use crate::api;
use crate::components::library_common::{bytes, on_entity_changed};
use crate::components::library_forms::{opt_from, opt_key};
use crate::components::radio::FOLLOW;
use crate::components::settings_draft::{SettingsDraft, loaded_view, save_button};
use crate::components::{Card, EmptyState, ErrorBox, Icon, PageHeader, Skeleton};
use crate::pages::system::require_admin;

const NAMING_TOKENS: &[(&str, &[&str])] = &[
    ("Show", &["{Show Title}", "{Show CleanTitle}", "{Station Name}"]),
    (
        "Episode",
        &[
            "{Series:00}",
            "{Episode:00}",
            "{Episode Title}",
            "{Air Date}",
            "{Air Year}",
        ],
    ),
    ("File", &["{Quality Title}"]),
];

type Getter = fn(&RadioNamingDto) -> String;
type Setter = fn(&mut RadioNamingDto, String);

const NAMING_FIELDS: &[(&str, &str, &str, Getter, Setter)] = &[
    (
        "show_folder",
        "Show folder",
        "Folder of a show under the radio root folder (or the podcast library folder).",
        |n| n.show_folder.clone(),
        |n, v| n.show_folder = v,
    ),
    (
        "episode_file",
        "Episode file",
        "File name without the extension. Specials are series 0.",
        |n| n.episode_file.clone(),
        |n, v| n.episode_file = v,
    ),
];

fn text(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

fn opt_text(v: String) -> Option<String> {
    let t = v.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[component]
pub fn RadioSettingsPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Radio" subtitle="The Audiobookshelf podcast library, how new shows are followed and which sources are used." />
        <div class="stack">
            <RadioSettingsCard />
            <RadioRootsCard />
        </div>
    }
}

#[component]
fn RadioSettingsCard() -> impl IntoView {
    let s = SettingsDraft::<RadioSettingsDto>::new("/radio/settings");
    let providers = LocalResource::new(|| api::get::<Vec<RadioProviderDto>>("/radio/providers"));
    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        s.save("Radio settings", |fresh, d| *fresh = d.clone());
    };

    let provider_list = move || match providers.get() {
        None => view! { <Skeleton lines=2 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
        Some(Ok(list)) => {
            let ids: Vec<String> = list.iter().map(|p| p.id.clone()).collect();
            let ids = StoredValue::new(ids);
            view! {
                <fieldset class="defaults-editor">
                    <legend class="field-label">"Sources"</legend>
                    {list.into_iter().map(|p| {
                        let pid = p.id.clone();
                        let pid2 = pid.clone();
                        // An empty list means every source.
                        let on = move || s.get(|d| d.enabled_providers.is_empty() || d.enabled_providers.contains(&pid));
                        view! {
                            <label class="checkbox">
                                <input type="checkbox" prop:checked=on on:change=move |e| {
                                    let checked = event_target_checked(&e);
                                    let all = ids.get_value();
                                    s.edit(|d| {
                                        let mut list = if d.enabled_providers.is_empty() { all.clone() } else { d.enabled_providers.clone() };
                                        list.retain(|x| *x != pid2);
                                        if checked { list.push(pid2.clone()); }
                                        list.sort();
                                        d.enabled_providers = if all.iter().all(|a| list.contains(a)) { Vec::new() } else { list };
                                    });
                                } />
                                {p.name.clone()}
                                <span class="field-hint">{if p.supports_search { " — search by name and links" } else { " — links only" }}</span>
                            </label>
                        }
                    }).collect_view()}
                    <span class="field-hint">"All sources are used while every box is ticked."</span>
                </fieldset>
            }
            .into_any()
        }
    };

    let form = move || {
        view! {
            <form class="form" on:submit=save>
                <h3 class="form-section">"Audiobookshelf podcast library"</h3>
                <div class="field">
                    <label for="rs-lib">"Podcast library id"</label>
                    <input id="rs-lib" class="input mono" type="text" spellcheck="false"
                        prop:value=move || s.get(|d| text(&d.abs_library_id))
                        on:input=move |e| { let v = opt_text(event_target_value(&e)); s.edit(|d| d.abs_library_id = v); } />
                    <span class="field-hint">"The id of the podcast library in Audiobookshelf (from its library settings URL). The Audiobookshelf server itself is set up under Settings → Media Servers."</span>
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="rs-absfolder">"Library folder in Audiobookshelf"</label>
                        <input id="rs-absfolder" class="input mono" type="text" spellcheck="false" placeholder="/podcasts"
                            prop:value=move || s.get(|d| text(&d.abs_library_folder))
                            on:input=move |e| { let v = opt_text(event_target_value(&e)); s.edit(|d| d.abs_library_folder = v); } />
                        <span class="field-hint">"The folder as Audiobookshelf sees it (inside its container)."</span>
                    </div>
                    <div class="field">
                        <label for="rs-folder">"Same folder in Reelix"</label>
                        <input id="rs-folder" class="input mono" type="text" spellcheck="false" placeholder="/media/podcasts"
                            prop:value=move || s.get(|d| text(&d.library_folder))
                            on:input=move |e| { let v = opt_text(event_target_value(&e)); s.edit(|d| d.library_folder = v); } />
                        <span class="field-hint">"Must exist and be writable by Reelix."</span>
                    </div>
                </div>
                <h3 class="form-section">"New shows"</h3>
                <div class="field">
                    <label for="rs-follow">"Default follow choice"</label>
                    <select id="rs-follow" class="input" on:change=move |e| {
                        if let Some(f) = opt_from(&FOLLOW, &event_target_value(&e)) { s.edit(|d| d.default_follow = f); }
                    }>
                        {FOLLOW.iter().map(|(_, k, l)| {
                            let k: &'static str = k;
                            view! { <option value=k selected=move || s.get(|d| opt_key(&FOLLOW, d.default_follow)) == k>{*l}</option> }
                        }).collect_view()}
                    </select>
                </div>
                <h3 class="form-section">"Sources and search"</h3>
                {provider_list}
                <div class="field-row">
                    <div class="field">
                        <label for="rs-country">"Country"</label>
                        <input id="rs-country" class="input" type="text" maxlength="2" placeholder="GB" autocomplete="off"
                            prop:value=move || s.get(|d| text(&d.country))
                            on:input=move |e| { let v = opt_text(event_target_value(&e).to_uppercase()); s.edit(|d| d.country = v); } />
                        <span class="field-hint">"ISO 3166 code preferred by searches."</span>
                    </div>
                    <div class="field">
                        <label for="rs-language">"Language"</label>
                        <input id="rs-language" class="input" type="text" maxlength="3" placeholder="en" autocomplete="off"
                            prop:value=move || s.get(|d| text(&d.language))
                            on:input=move |e| { let v = opt_text(event_target_value(&e).to_lowercase()); s.edit(|d| d.language = v); } />
                        <span class="field-hint">"ISO 639 code preferred by searches."</span>
                    </div>
                    <div class="field">
                        <label for="rs-lookups">"Lookups per refresh"</label>
                        <input id="rs-lookups" class="input" type="number" min="0" max="1000"
                            prop:value=move || s.get(|d| d.max_lookups_per_refresh.to_string())
                            on:input=move |e| {
                                if let Ok(v) = event_target_value(&e).trim().parse::<u32>() { s.edit(|d| d.max_lookups_per_refresh = v); }
                            } />
                        <span class="field-hint">"Per-episode requests (lengths, details) one refresh may make per show."</span>
                    </div>
                </div>
                {save_button(s.saving)}
            </form>
        }
    };

    view! {
        <Card title="Radio settings">{loaded_view(s, 8, form)}</Card>
    }
}

/// Read-only list of the radio root folders, managed with the others in Media management.
#[component]
fn RadioRootsCard() -> impl IntoView {
    let roots = LocalResource::new(|| api::get::<Vec<ContentRootFolderDto>>("/radio/rootfolders"));
    on_entity_changed(&["rootfolder"], false, move || roots.refetch());
    let body = move || {
        match roots.get() {
        None => view! { <Skeleton lines=2 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| roots.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState icon="folder" title="No radio root folders" compact=true
                description="Without one, shows use the library folder above." />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <ul class="check-list">
                {list.into_iter().map(|r| view! {
                    <li class="mono">
                        {r.path.clone()}
                        {r.free_space.map(|f| view! { <span class="muted">{format!(" — {} free", bytes(f))}</span> })}
                    </li>
                }).collect_view()}
            </ul>
        }
        .into_any(),
    }
    };
    view! {
        <Card title="Root folders" subtitle="Where filed radio episodes go, as Reelix sees them."
            actions=view! {
                <A href="/settings/mediamanagement" attr:class="btn btn-sm btn-secondary"><Icon name="folder" />"Manage in Media management"</A>
            }.into_any()>
            {body}
        </Card>
    }
}

/// Folder and file naming of filed radio episodes with a token cheat-sheet (shown on Naming).
#[component]
pub fn RadioNamingCard() -> impl IntoView {
    let s = SettingsDraft::<RadioNamingDto>::new("/radio/naming");
    let focused = RwSignal::new("episode_file");

    let insert_token = move |tok: &'static str| {
        let field = focused.get_untracked();
        if let Some((_, _, _, get, set)) = NAMING_FIELDS.iter().find(|f| f.0 == field) {
            s.edit(|d| {
                let mut v = get(d);
                if !v.is_empty() && !v.ends_with(' ') {
                    v.push(' ');
                }
                v.push_str(tok);
                set(d, v);
            });
        }
    };
    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        s.save("Radio naming", |fresh, d| *fresh = d.clone());
    };

    let form = move || {
        view! {
            <form class="form" on:submit=save>
                {NAMING_FIELDS.iter().map(|&(id, label, hint, get, set)| view! {
                    <div class="field">
                        <label for=format!("rn-{id}")>{label}</label>
                        <input id=format!("rn-{id}") class="input mono" type="text" spellcheck="false"
                            prop:value=move || s.get(get)
                            on:focus=move |_| focused.set(id)
                            on:input=move |e| { let v = event_target_value(&e); s.edit(|d| set(d, v)); } />
                        <span class="field-hint">{hint}</span>
                    </div>
                }).collect_view()}
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || s.get(|d| d.write_tags)
                        on:change=move |e| { let v = event_target_checked(&e); s.edit(|d| d.write_tags = v); } />
                    "Write tags into filed episodes (show, title, series, episode, date, station)"
                </label>
                <p class="field-hint">
                    "Tags are written into a copy of its own, so a download that is still seeding is never changed; turn this off to hard link instead."
                </p>
                {save_button(s.saving)}
            </form>
        }
    };

    view! {
        <div class="split-grid naming-grid">
            <Card title="Radio" subtitle="Episodes Reelix files into the podcast library.">{loaded_view(s, 4, form)}</Card>
            <Card title="Radio tokens" subtitle="Click a token to add it to the last radio field you edited.">
                <div class="stack">
                    {NAMING_TOKENS.iter().map(|(group, toks)| view! {
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
                    <p class="field-hint">"{Air Date} is the first broadcast as 2026-11-04. The defaults file episodes as {Show Title}/{Show Title} - S20E03 - {Episode Title}."</p>
                </div>
            </Card>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_fields_read_and_write_their_template() {
        let mut n = RadioNamingDto::default();
        for (id, _, _, get, set) in NAMING_FIELDS {
            set(&mut n, format!("x-{id}"));
            assert_eq!(get(&n), format!("x-{id}"));
        }
        assert_eq!(n.show_folder, "x-show_folder");
        assert_eq!(n.episode_file, "x-episode_file");
    }
}
