//! `/settings/subtitles` — subtitles from OpenSubtitles and Stremio subtitle add-ons, saved
//! beside films and episodes. Off until activated (fork issue #62).

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::subtitles::{OpenSubtitlesSettings, SubtitleLanguage, SubtitleSettings};
use wasm_bindgen::JsCast;

use crate::api;
use crate::components::{Card, ErrorBox, PageHeader, Skeleton};
use crate::pages::system::require_admin;
use crate::state::Toasts;

#[component]
pub fn SubtitlesPage() -> impl IntoView {
    require_admin();
    let settings = LocalResource::new(|| api::get::<SubtitleSettings>("/settings/subtitles"));
    view! {
        <PageHeader title="Subtitles" subtitle="Subtitles from OpenSubtitles and Stremio subtitle add-ons, saved beside each film and episode for Jellyfin." />
        {move || match settings.get() {
            None => view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) /> }.into_any(),
            Some(Ok(s)) => view! { <SubtitlesForm initial=s /> }.into_any(),
        }}
    }
}

#[derive(Clone, Copy)]
struct LangRow {
    key: usize,
    code: RwSignal<String>,
    normal: RwSignal<bool>,
    sdh: RwSignal<bool>,
    forced: RwSignal<bool>,
}

impl LangRow {
    fn new(key: usize, l: &SubtitleLanguage) -> Self {
        Self {
            key,
            code: RwSignal::new(l.code.clone()),
            normal: RwSignal::new(l.normal),
            sdh: RwSignal::new(l.sdh),
            forced: RwSignal::new(l.forced),
        }
    }

    fn get(&self) -> SubtitleLanguage {
        SubtitleLanguage {
            code: self.code.get_untracked().trim().to_owned(),
            normal: self.normal.get_untracked(),
            sdh: self.sdh.get_untracked(),
            forced: self.forced.get_untracked(),
        }
    }
}

/// Moves focus to the element with `id` once the page has drawn it.
fn focus_later(id: String) {
    gloo_timers::callback::Timeout::new(0, move || {
        if let Some(el) = document()
            .get_element_by_id(&id)
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = el.focus();
        }
    })
    .forget();
}

#[component]
fn Check(
    value: RwSignal<bool>,
    #[prop(into)] label: String,
    #[prop(optional, into)] hint: Option<String>,
) -> impl IntoView {
    view! {
        <label class="checkbox toggle-row">
            <input type="checkbox" prop:checked=move || value.get() on:change=move |e| value.set(event_target_checked(&e)) />
            <span>
                <span class="perm-label">{label}</span>
                {hint.map(|h| view! { <span class="perm-desc">{h}</span> })}
            </span>
        </label>
    }
}

#[component]
fn SubtitlesForm(initial: SubtitleSettings) -> impl IntoView {
    let toasts = Toasts::expect();
    let enabled = RwSignal::new(initial.enabled);
    let after_import = RwSignal::new(initial.fetch_after_import);
    let search_missing = RwSignal::new(initial.search_missing);
    let upgrade = RwSignal::new(initial.upgrade);
    let next_key = StoredValue::new(initial.languages.len());
    let rows = RwSignal::new(
        initial
            .languages
            .iter()
            .enumerate()
            .map(|(i, l)| LangRow::new(i, l))
            .collect::<Vec<_>>(),
    );
    let os = initial.opensubtitles.clone();
    let os_on = RwSignal::new(os.enabled);
    let os_key = RwSignal::new(os.api_key.clone());
    let os_user = RwSignal::new(os.username.clone());
    let os_password = RwSignal::new(os.password.clone());
    let addons = RwSignal::new(initial.stremio_addons.join("\n"));
    let busy = RwSignal::new(false);

    let add_language = move |_| {
        let key = next_key.get_value();
        next_key.set_value(key + 1);
        rows.update(|r| {
            r.push(LangRow::new(
                key,
                &SubtitleLanguage {
                    code: String::new(),
                    normal: true,
                    sdh: false,
                    forced: false,
                },
            ))
        });
        focus_later(format!("sub-lang-{key}"));
    };

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let body = SubtitleSettings {
            enabled: enabled.get_untracked(),
            languages: rows.get_untracked().iter().map(LangRow::get).collect(),
            opensubtitles: OpenSubtitlesSettings {
                enabled: os_on.get_untracked(),
                api_key: os_key.get_untracked(),
                username: os_user.get_untracked().trim().to_owned(),
                password: os_password.get_untracked(),
            },
            stremio_addons: addons
                .get_untracked()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect(),
            fetch_after_import: after_import.get_untracked(),
            search_missing: search_missing.get_untracked(),
            upgrade: upgrade.get_untracked(),
        };
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, SubtitleSettings>("/settings/subtitles", &body).await {
                Ok(saved) => {
                    os_key.set(saved.opensubtitles.api_key.clone());
                    os_password.set(saved.opensubtitles.password.clone());
                    addons.set(saved.stremio_addons.join("\n"));
                    rows.set(
                        saved
                            .languages
                            .iter()
                            .enumerate()
                            .map(|(i, l)| LangRow::new(next_key.get_value() + i, l))
                            .collect(),
                    );
                    next_key.set_value(next_key.get_value() + saved.languages.len());
                    toasts.success("Subtitle settings saved.");
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            busy.set(false);
        });
    };

    view! {
        <form class="form" autocomplete="off" data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other" on:submit=save>
            <Card title="Subtitles" subtitle="Off until activated here. Reelix asks the sources only while this is on.">
                <div class="form">
                    <Check value=enabled label="Fetch subtitles" hint="Saved beside the video as Jellyfin reads them: Name (Year).en.srt, .en.sdh.srt and .en.forced.srt." />
                    <Check value=after_import label="Search after each import" hint="As soon as a film or episode is imported." />
                    <Check value=search_missing label="Search for missing subtitles on a schedule" hint="Every 6 hours, at most 50 files a run. A file is searched again after a day while a subtitle is missing." />
                    <Check value=upgrade label="Replace with a subtitle made for the file" hint="When a subtitle that matches the file itself (a file-hash match) appears later, it replaces one matched by title or release name." />
                </div>
            </Card>
            <Card title="Languages" subtitle="In order of preference. Use two-letter codes, such as en, or with a region, such as pt-BR.">
                <div class="form">
                    <For each=move || rows.get() key=|r| r.key let:row>
                        {
                            let id = format!("sub-lang-{}", row.key);
                            let remove = move |_| {
                                rows.update(|r| r.retain(|x| x.key != row.key));
                                focus_later("sub-add-lang".into());
                            };
                            view! {
                                <div class="field-row" role="group" aria-label=move || format!("Language {}", row.code.get())>
                                    <div class="field">
                                        <label for=id.clone()>"Language code"</label>
                                        <input id=id class="input" maxlength="5" placeholder="en" autocomplete="off"
                                            prop:value=move || row.code.get() on:input=move |e| row.code.set(event_target_value(&e)) />
                                    </div>
                                    <Check value=row.normal label="Normal" />
                                    <Check value=row.sdh label="SDH" hint="For people who are deaf or hard of hearing." />
                                    <Check value=row.forced label="Forced" hint="Only the parts in another language." />
                                    <div class="form-actions">
                                        <button type="button" class="btn btn-secondary" on:click=remove
                                            aria-label=move || format!("Remove language {}", row.code.get())>"Remove"</button>
                                    </div>
                                </div>
                            }
                        }
                    </For>
                    <Show when=move || rows.with(Vec::is_empty)>
                        <p class="field-hint">"No languages yet."</p>
                    </Show>
                    <div class="form-actions">
                        <button id="sub-add-lang" type="button" class="btn btn-secondary" on:click=add_language>"Add language"</button>
                    </div>
                </div>
            </Card>
            <Card title="OpenSubtitles" subtitle="Your own API key and account from opensubtitles.com. A free account allows a small number of downloads a day.">
                <div class="form">
                    <Check value=os_on label="Use OpenSubtitles" />
                    <div class="field">
                        <label for="sub-os-key">"API key"</label>
                        <input id="sub-os-key" class="input" type="password" autocomplete="new-password" placeholder="Not set"
                            data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                            prop:value=move || os_key.get() on:input=move |e| os_key.set(event_target_value(&e)) />
                        <p class="field-hint">"From opensubtitles.com, Profile, API consumers."</p>
                    </div>
                    <div class="field-row">
                        <div class="field">
                            <label for="sub-os-user">"Username"</label>
                            <input id="sub-os-user" class="input" autocomplete="off"
                                data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                                prop:value=move || os_user.get() on:input=move |e| os_user.set(event_target_value(&e)) />
                        </div>
                        <div class="field">
                            <label for="sub-os-password">"Password"</label>
                            <input id="sub-os-password" class="input" type="password" autocomplete="new-password"
                                data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                                prop:value=move || os_password.get() on:input=move |e| os_password.set(event_target_value(&e)) />
                        </div>
                    </div>
                </div>
            </Card>
            <Card title="Stremio subtitle add-ons" subtitle="Optional. One add-on address a line, such as https://example.org/manifest.json. Add-ons supply normal subtitles only.">
                <div class="field">
                    <label for="sub-addons">"Add-on addresses"</label>
                    <textarea id="sub-addons" class="input textarea mono" rows="3"
                        prop:value=move || addons.get() on:input=move |e| addons.set(event_target_value(&e))></textarea>
                </div>
            </Card>
            <div class="form-actions sticky-actions">
                <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                    {move || if busy.get() { "Saving…" } else { "Save changes" }}
                </button>
            </div>
        </form>
    }
}
