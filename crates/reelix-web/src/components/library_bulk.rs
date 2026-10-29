//! Bulk editor (and single-item editor) for movies and series.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::library::{
    MediaKind, MinimumAvailability, MovieBulkEdit, MovieEdit, SeriesBulkEdit, SeriesEdit, SeriesType, TagMode,
};

use super::library_forms::{
    LibOptions, MIN_AVAILABILITY, ProfileSelect, RootFolderSelect, SERIES_TYPES, TagPicker, opt_from,
};
use super::{Icon, Modal, Spinner};
use crate::api;
use crate::state::Toasts;

/// Edits `ids` in one request. Fields left at "No change" are not sent.
#[component]
pub fn BulkEditModal(
    kind: MediaKind,
    open: RwSignal<bool>,
    #[prop(into)] ids: Signal<Vec<i64>>,
    opts: LibOptions,
    on_saved: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let monitored = RwSignal::new(String::new());
    let profile = RwSignal::new(0i64);
    let root = RwSignal::new(0i64);
    let move_files = RwSignal::new(false);
    let min_avail = RwSignal::new(String::new());
    let series_type = RwSignal::new(String::new());
    let season_folder = RwSignal::new(String::new());
    let change_tags = RwSignal::new(false);
    let tag_mode = RwSignal::new(TagMode::Add);
    let tags = RwSignal::new(Vec::<i64>::new());
    let busy = RwSignal::new(false);

    Effect::new(move |_| {
        if open.get() {
            monitored.set(String::new());
            profile.set(0);
            root.set(0);
            move_files.set(false);
            min_avail.set(String::new());
            series_type.set(String::new());
            season_folder.set(String::new());
            change_tags.set(false);
            tags.set(Vec::new());
        }
    });

    let tri = |s: &str| match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let ids = ids.get_untracked();
        let n = ids.len();
        let mon = tri(&monitored.get_untracked());
        let qp = Some(profile.get_untracked()).filter(|p| *p != 0);
        let rf = Some(root.get_untracked()).filter(|r| *r != 0);
        let tg = change_tags.get_untracked().then(|| tags.get_untracked());
        let mode = tag_mode.get_untracked();
        let mv = move_files.get_untracked();
        busy.set(true);
        spawn_local(async move {
            let res = match kind {
                MediaKind::Movie => {
                    let body = MovieBulkEdit {
                        ids,
                        edit: MovieEdit {
                            monitored: mon,
                            quality_profile_id: qp,
                            minimum_availability: opt_from(&MIN_AVAILABILITY, &min_avail.get_untracked()),
                            root_folder_id: rf,
                            path: None,
                            move_files: mv,
                            tags: tg,
                            tag_mode: mode,
                        },
                    };
                    api::put::<_, serde_json::Value>("/movies/bulk", &body).await
                }
                MediaKind::Series => {
                    let body = SeriesBulkEdit {
                        ids,
                        edit: SeriesEdit {
                            monitored: mon,
                            quality_profile_id: qp,
                            series_type: opt_from(&SERIES_TYPES, &series_type.get_untracked()),
                            season_folder: tri(&season_folder.get_untracked()),
                            monitor_new_items: None,
                            root_folder_id: rf,
                            path: None,
                            move_files: mv,
                            tags: tg,
                            tag_mode: mode,
                        },
                    };
                    api::put::<_, serde_json::Value>("/series/bulk", &body).await
                }
            };
            busy.set(false);
            match res {
                Ok(_) => {
                    toasts.success(format!("Updated {n} item(s)."));
                    open.set(false);
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };

    let tri_select = |id: &'static str, sig: RwSignal<String>, yes: &'static str, no: &'static str| {
        view! {
            <select id=id class="input" on:change=move |e| sig.set(event_target_value(&e))>
                <option value="" selected=move || sig.get().is_empty()>"No change"</option>
                <option value="true" selected=move || sig.get() == "true">{yes}</option>
                <option value="false" selected=move || sig.get() == "false">{no}</option>
            </select>
        }
    };

    view! {
        <Modal open=open title="Edit selected">
            <form class="form" on:submit=submit>
                <p class="modal-text">{move || format!("Changes apply to {} item(s). Fields left at “No change” keep their value.", ids.get().len())}</p>
                <div class="field-row">
                    <div class="field">
                        <label for="be-mon">"Monitored"</label>
                        {tri_select("be-mon", monitored, "Monitored", "Unmonitored")}
                    </div>
                    <div class="field">
                        <label for="be-profile">"Quality profile"</label>
                        <ProfileSelect opts=opts value=profile id="be-profile" none_label="No change" />
                    </div>
                </div>
                <div class="field-row">
                    {match kind {
                        MediaKind::Movie => view! {
                            <div class="field">
                                <label for="be-avail">"Minimum availability"</label>
                                <select id="be-avail" class="input" on:change=move |e| min_avail.set(event_target_value(&e))>
                                    <option value="" selected=move || min_avail.get().is_empty()>"No change"</option>
                                    {MIN_AVAILABILITY.iter().map(|(_, k, l)| view! {
                                        <option value=*k selected=move || min_avail.get() == *k>{*l}</option>
                                    }).collect_view()}
                                </select>
                            </div>
                        }.into_any(),
                        MediaKind::Series => view! {
                            <div class="field">
                                <label for="be-type">"Series type"</label>
                                <select id="be-type" class="input" on:change=move |e| series_type.set(event_target_value(&e))>
                                    <option value="" selected=move || series_type.get().is_empty()>"No change"</option>
                                    {SERIES_TYPES.iter().map(|(_, k, l)| view! {
                                        <option value=*k selected=move || series_type.get() == *k>{*l}</option>
                                    }).collect_view()}
                                </select>
                            </div>
                            <div class="field">
                                <label for="be-sf">"Season folders"</label>
                                {tri_select("be-sf", season_folder, "Yes", "No")}
                            </div>
                        }.into_any(),
                    }}
                </div>
                <div class="field">
                    <label for="be-root">"Root folder"</label>
                    <RootFolderSelect opts=opts kind=kind value=root id="be-root" none_label="No change" />
                    <Show when=move || root.get() != 0>
                        <label class="checkbox">
                            <input type="checkbox" prop:checked=move || move_files.get() on:change=move |e| move_files.set(event_target_checked(&e)) />
                            "Move existing files to the new root folder"
                        </label>
                    </Show>
                </div>
                <div class="field">
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || change_tags.get() on:change=move |e| change_tags.set(event_target_checked(&e)) />
                        "Change tags"
                    </label>
                    <Show when=move || change_tags.get()>
                        <div class="segmented" role="group" aria-label="Tag mode">
                            {[(TagMode::Add, "Add"), (TagMode::Remove, "Remove"), (TagMode::Replace, "Replace")].into_iter().map(|(m, l)| view! {
                                <button type="button" class="seg-btn" class:active=move || tag_mode.get() == m
                                    aria-pressed=move || (tag_mode.get() == m).to_string() on:click=move |_| tag_mode.set(m)>{l}</button>
                            }).collect_view()}
                        </div>
                        <TagPicker opts=opts value=tags />
                    </Show>
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {move || busy.get().then(|| view! { <Spinner /> })}
                        "Save"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Fields of the single-item editor.
#[derive(Clone, Copy)]
pub struct EditFields {
    pub monitored: RwSignal<bool>,
    pub profile: RwSignal<i64>,
    pub min_avail: RwSignal<MinimumAvailability>,
    pub series_type: RwSignal<SeriesType>,
    pub season_folder: RwSignal<bool>,
    pub path: RwSignal<String>,
    pub move_files: RwSignal<bool>,
    pub tags: RwSignal<Vec<i64>>,
}

impl EditFields {
    pub fn new() -> Self {
        Self {
            monitored: RwSignal::new(true),
            profile: RwSignal::new(0),
            min_avail: RwSignal::new(MinimumAvailability::Released),
            series_type: RwSignal::new(SeriesType::Standard),
            season_folder: RwSignal::new(true),
            path: RwSignal::new(String::new()),
            move_files: RwSignal::new(false),
            tags: RwSignal::new(Vec::new()),
        }
    }
}

/// Single movie / series edit dialog. The caller fills `fields` before opening.
#[component]
pub fn EditMediaModal(
    kind: MediaKind,
    id: i64,
    open: RwSignal<bool>,
    fields: EditFields,
    #[prop(into)] original_path: Signal<String>,
    opts: LibOptions,
    on_saved: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let busy = RwSignal::new(false);
    let f = fields;
    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let path = f.path.get_untracked().trim().to_string();
        let path = (!path.is_empty() && path != original_path.get_untracked()).then_some(path);
        let moving = path.is_some() && f.move_files.get_untracked();
        busy.set(true);
        spawn_local(async move {
            let res = match kind {
                MediaKind::Movie => {
                    let body = MovieEdit {
                        monitored: Some(f.monitored.get_untracked()),
                        quality_profile_id: Some(f.profile.get_untracked()).filter(|p| *p != 0),
                        minimum_availability: Some(f.min_avail.get_untracked()),
                        root_folder_id: None,
                        path,
                        move_files: moving,
                        tags: Some(f.tags.get_untracked()),
                        tag_mode: TagMode::Replace,
                    };
                    api::put::<_, serde_json::Value>(&format!("/movies/{id}"), &body).await
                }
                MediaKind::Series => {
                    let body = SeriesEdit {
                        monitored: Some(f.monitored.get_untracked()),
                        quality_profile_id: Some(f.profile.get_untracked()).filter(|p| *p != 0),
                        series_type: Some(f.series_type.get_untracked()),
                        season_folder: Some(f.season_folder.get_untracked()),
                        monitor_new_items: None,
                        root_folder_id: None,
                        path,
                        move_files: moving,
                        tags: Some(f.tags.get_untracked()),
                        tag_mode: TagMode::Replace,
                    };
                    api::put::<_, serde_json::Value>(&format!("/series/{id}"), &body).await
                }
            };
            busy.set(false);
            match res {
                Ok(_) => {
                    toasts.success("Saved.");
                    open.set(false);
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };
    view! {
        <Modal open=open title="Edit">
            <form class="form" on:submit=submit>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || f.monitored.get() on:change=move |e| f.monitored.set(event_target_checked(&e)) />
                    "Monitored"
                </label>
                <div class="field-row">
                    <div class="field">
                        <label for="ed-profile">"Quality profile"</label>
                        <ProfileSelect opts=opts value=f.profile id="ed-profile" />
                    </div>
                    {match kind {
                        MediaKind::Movie => view! {
                            <div class="field">
                                <label for="ed-avail">"Minimum availability"</label>
                                <super::library_forms::EnumSelect table=&MIN_AVAILABILITY value=f.min_avail id="ed-avail" />
                            </div>
                        }.into_any(),
                        MediaKind::Series => view! {
                            <div class="field">
                                <label for="ed-type">"Series type"</label>
                                <super::library_forms::EnumSelect table=&SERIES_TYPES value=f.series_type id="ed-type" />
                            </div>
                        }.into_any(),
                    }}
                </div>
                {(kind == MediaKind::Series).then(|| view! {
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || f.season_folder.get() on:change=move |e| f.season_folder.set(event_target_checked(&e)) />
                        "Use season folders"
                    </label>
                })}
                <div class="field">
                    <label for="ed-path">"Path"</label>
                    <input id="ed-path" class="input mono" type="text" prop:value=move || f.path.get()
                        on:input=move |e| f.path.set(event_target_value(&e)) />
                    <Show when=move || f.path.get().trim() != original_path.get()>
                        <label class="checkbox">
                            <input type="checkbox" prop:checked=move || f.move_files.get() on:change=move |e| f.move_files.set(event_target_checked(&e)) />
                            "Move existing files to the new path"
                        </label>
                    </Show>
                </div>
                <div class="field">
                    <span class="field-label">"Tags"</span>
                    <TagPicker opts=opts value=f.tags />
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="check" /> }.into_any() }}
                        "Save"
                    </button>
                </div>
            </form>
        </Modal>
    }
}
