//! `/settings/naming` — file and folder naming templates with a token cheat-sheet and live preview,
//! followed by the book and music templates.

use std::time::Duration;

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::library::{ColonReplacement, MultiEpisodeStyle, NamingPreviewDto, NamingSettings};

use crate::api;
use crate::components::library_forms::EnumSelect;
use crate::components::{Card, ErrorBox, Icon, PageHeader, Skeleton, Spinner};
use crate::pages::books::BookNamingCard;
use crate::pages::music::MusicNamingCard;
use crate::pages::system::require_admin;
use crate::state::Toasts;

const COLON: [(ColonReplacement, &str, &str); 5] = [
    (ColonReplacement::Smart, "smart", "Smart replace"),
    (ColonReplacement::Delete, "delete", "Delete"),
    (ColonReplacement::Dash, "dash", "Replace with dash"),
    (
        ColonReplacement::SpaceDash,
        "space_dash",
        "Replace with space dash",
    ),
    (
        ColonReplacement::SpaceDashSpace,
        "space_dash_space",
        "Replace with space dash space",
    ),
];

const MULTI: [(MultiEpisodeStyle, &str, &str); 6] = [
    (MultiEpisodeStyle::Extend, "extend", "Extend (S01E01-02-03)"),
    (
        MultiEpisodeStyle::Duplicate,
        "duplicate",
        "Duplicate (S01E01.S01E02)",
    ),
    (MultiEpisodeStyle::Repeat, "repeat", "Repeat (S01E01E02E03)"),
    (MultiEpisodeStyle::Scene, "scene", "Scene (S01E01-E02-E03)"),
    (MultiEpisodeStyle::Range, "range", "Range (S01E01-03)"),
    (
        MultiEpisodeStyle::PrefixedRange,
        "prefixed_range",
        "Prefixed range (S01E01-E03)",
    ),
];

/// Token groups for the cheat-sheet.
const TOKENS: &[(&str, &[&str])] = &[
    (
        "Movie",
        &[
            "{Movie Title}",
            "{Movie CleanTitle}",
            "{Movie TitleThe}",
            "{Movie TitleYear}",
            "{Movie OriginalTitle}",
            "{Movie Collection}",
            "{Movie Certification}",
            "{Release Year}",
            "{Edition Tags}",
            "{TmdbId}",
            "{ImdbId}",
            "{tmdb-{TmdbId}}",
        ],
    ),
    (
        "Series",
        &[
            "{Series Title}",
            "{Series CleanTitle}",
            "{Series TitleYear}",
            "{Series TitleThe}",
            "{Series TitleWithoutYear}",
            "{Series Year}",
            "{TvdbId}",
            "{tvdb-{TvdbId}}",
        ],
    ),
    (
        "Episode",
        &[
            "{season:00}",
            "{episode:00}",
            "{absolute:000}",
            "{Episode Title}",
            "{Episode CleanTitle}",
            "{Air-Date}",
            "{Air Date}",
        ],
    ),
    (
        "Quality",
        &[
            "{Quality Full}",
            "{Quality Title}",
            "{Quality Proper}",
            "{Quality Real}",
        ],
    ),
    (
        "Media info",
        &[
            "{MediaInfo Simple}",
            "{MediaInfo Full}",
            "{MediaInfo VideoCodec}",
            "{MediaInfo VideoBitDepth}",
            "{MediaInfo VideoDynamicRange}",
            "{MediaInfo VideoDynamicRangeType}",
            "{MediaInfo AudioCodec}",
            "{MediaInfo AudioChannels}",
            "{MediaInfo AudioLanguages}",
            "{MediaInfo SubtitleLanguages}",
        ],
    ),
    (
        "Release",
        &[
            "{Release Group}",
            "{-Release Group}",
            "{Custom Formats}",
            "{Original Title}",
            "{Original Filename}",
        ],
    ),
];

type Getter = fn(&NamingSettings) -> String;
type Setter = fn(&mut NamingSettings, String);
type PreviewGetter = fn(&NamingPreviewDto) -> String;

/// The template fields: (id, label, getter, setter, preview getter).
const FIELDS: &[(&str, &str, Getter, Setter, PreviewGetter)] = &[
    (
        "movie_folder_format",
        "Movie folder",
        |s| s.movie_folder_format.clone(),
        |s, v| s.movie_folder_format = v,
        |p| p.movie_folder.clone(),
    ),
    (
        "movie_file_format",
        "Movie file",
        |s| s.movie_file_format.clone(),
        |s, v| s.movie_file_format = v,
        |p| p.movie_file.clone(),
    ),
    (
        "series_folder_format",
        "Series folder",
        |s| s.series_folder_format.clone(),
        |s, v| s.series_folder_format = v,
        |p| p.series_folder.clone(),
    ),
    (
        "season_folder_format",
        "Season folder",
        |s| s.season_folder_format.clone(),
        |s, v| s.season_folder_format = v,
        |p| p.season_folder.clone(),
    ),
    (
        "specials_folder_format",
        "Specials folder",
        |s| s.specials_folder_format.clone(),
        |s, v| s.specials_folder_format = v,
        |_| String::new(),
    ),
    (
        "standard_episode_format",
        "Standard episode",
        |s| s.standard_episode_format.clone(),
        |s, v| s.standard_episode_format = v,
        |p| format!("{}\n{}", p.standard_episode, p.multi_episode),
    ),
    (
        "daily_episode_format",
        "Daily episode",
        |s| s.daily_episode_format.clone(),
        |s, v| s.daily_episode_format = v,
        |p| p.daily_episode.clone(),
    ),
    (
        "anime_episode_format",
        "Anime episode",
        |s| s.anime_episode_format.clone(),
        |s, v| s.anime_episode_format = v,
        |p| p.anime_episode.clone(),
    ),
];

#[component]
pub fn NamingPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let loaded = LocalResource::new(|| api::get::<NamingSettings>("/settings/naming"));
    let draft = RwSignal::new(None::<NamingSettings>);
    let colon = RwSignal::new(ColonReplacement::Smart);
    let multi = RwSignal::new(MultiEpisodeStyle::Extend);
    let preview = RwSignal::new(None::<NamingPreviewDto>);
    let previewing = RwSignal::new(false);
    let saving = RwSignal::new(false);
    // Last focused template input, so cheat-sheet tokens can be inserted into it.
    let focused = RwSignal::new("movie_file_format");

    Effect::new(move |_| {
        if let Some(Ok(v)) = loaded.get() {
            colon.set(v.colon_replacement);
            multi.set(v.multi_episode_style);
            draft.set(Some(v));
        }
    });

    let current = move || {
        draft.get().map(|mut d| {
            d.colon_replacement = colon.get();
            d.multi_episode_style = multi.get();
            d
        })
    };

    // Debounced live preview of the unsaved draft.
    let seq = StoredValue::new(0u32);
    Effect::new(move |_| {
        let Some(d) = current() else { return };
        let n = seq.get_value() + 1;
        seq.set_value(n);
        set_timeout(
            move || {
                if seq.try_get_value() != Some(n) {
                    return;
                }
                previewing.set(true);
                spawn_local(async move {
                    if let Ok(p) = api::post::<_, NamingPreviewDto>("/naming/preview", &d).await {
                        preview.set(Some(p));
                    }
                    previewing.set(false);
                });
            },
            Duration::from_millis(350),
        );
    });

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(d) = current() else { return };
        if preview.with_untracked(|p| p.as_ref().is_some_and(|p| !p.errors.is_empty())) {
            toasts.error("Fix the template errors first.");
            return;
        }
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, NamingSettings>("/settings/naming", &d).await {
                Ok(saved) => {
                    toasts.success("Naming settings saved.");
                    draft.set(Some(saved));
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    let set_field = move |setter: Setter, v: String| {
        draft.update(|d| {
            if let Some(d) = d.as_mut() {
                setter(d, v)
            }
        })
    };
    let insert_token = move |tok: &'static str| {
        let field = focused.get_untracked();
        if let Some((_, _, get, set, _)) = FIELDS.iter().find(|f| f.0 == field)
            && let Some(d) = draft.get_untracked()
        {
            let mut v = get(&d);
            if !v.is_empty() && !v.ends_with(' ') {
                v.push(' ');
            }
            v.push_str(tok);
            set_field(*set, v);
        }
    };

    let field_error = move |id: &'static str| {
        preview.with(|p| {
            p.as_ref()
                .and_then(|p| p.errors.iter().find(|e| e.field == id).map(|e| e.message.clone()))
        })
    };

    let form = move || {
        match loaded.get() {
        None => view! { <Skeleton lines=8 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| loaded.refetch()) /> }.into_any(),
        Some(Ok(_)) => view! {
            <form class="form" on:submit=save>
                <div class="field-row">
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || draft.with(|d| d.as_ref().is_some_and(|d| d.rename_files))
                            on:change=move |e| { let v = event_target_checked(&e); draft.update(|d| if let Some(d) = d.as_mut() { d.rename_files = v }); } />
                        "Rename files on import"
                    </label>
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || draft.with(|d| d.as_ref().is_some_and(|d| d.replace_illegal_characters))
                            on:change=move |e| { let v = event_target_checked(&e); draft.update(|d| if let Some(d) = d.as_mut() { d.replace_illegal_characters = v }); } />
                        "Replace illegal characters"
                    </label>
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="nm-colon">"Colon replacement"</label>
                        <EnumSelect table=&COLON value=colon id="nm-colon" />
                    </div>
                    <div class="field">
                        <label for="nm-multi">"Multi-episode style"</label>
                        <EnumSelect table=&MULTI value=multi id="nm-multi" />
                    </div>
                </div>
                {FIELDS.iter().map(|&(id, label, get, set, pv)| {
                    view! {
                        <div class="field" class:invalid=move || field_error(id).is_some()>
                            <label for=format!("nm-{id}")>{label}</label>
                            <input id=format!("nm-{id}") class="input mono" type="text" spellcheck="false"
                                prop:value=move || draft.with(|d| d.as_ref().map(get).unwrap_or_default())
                                on:focus=move |_| focused.set(id)
                                on:input=move |e| set_field(set, event_target_value(&e)) />
                            {move || match field_error(id) {
                                Some(err) => view! { <span class="field-hint">{err}</span> }.into_any(),
                                None => {
                                    let text = preview.with(|p| p.as_ref().map(pv).unwrap_or_default());
                                    (!text.is_empty()).then(|| view! { <div class="preview-box">{text}</div> }).into_any()
                                }
                            }}
                        </div>
                    }
                }).collect_view()}
                <div class="modal-footer">
                    {move || previewing.get().then(|| view! { <Spinner label="Updating preview" /> })}
                    <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                        <Icon name="check" />
                        "Save changes"
                    </button>
                </div>
            </form>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="Naming" subtitle="How Reelix names files and folders. The preview uses sample data and updates as you type." />
        <div class="split-grid naming-grid">
            <Card title="Movies and series">{form}</Card>
            <Card title="Tokens" subtitle="Click a token to add it to the last field you edited.">
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
                    <p class="field-hint">"Wrap optional text in the braces, e.g. {-Release Group} adds the dash only when a group is known. {season:00} pads to two digits."</p>
                </div>
            </Card>
        </div>
        <BookNamingCard />
        <MusicNamingCard />
    }
}
