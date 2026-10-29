//! `/settings/customformats` — list, condition builder, TRaSH JSON import/export and a
//! live "test a release name" box (evaluated in the browser with the same engine as the server).

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::profiles::{CustomFormat, FormatSpecification, ImportedFormatDto, Language};
use reelix_decision::FormatInput;
use serde_json::{Value, json};

use crate::api;
use crate::components::{
    Card, ConfirmDialog, CopyButton, EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton,
};
use crate::pages::system::require_admin;
use crate::state::Toasts;

/// (kind, label) for the condition-type select.
const KINDS: &[(&str, &str)] = &[
    ("release_title", "Release title (regex)"),
    ("release_group", "Release group (regex)"),
    ("edition", "Edition (regex)"),
    ("source", "Source"),
    ("resolution", "Resolution"),
    ("quality_modifier", "Quality modifier"),
    ("language", "Language"),
    ("indexer_flag", "Indexer flag"),
    ("size", "Size (GB)"),
    ("release_type", "Release type"),
    ("year", "Year"),
];

const SOURCES: &[(&str, &str)] = &[
    ("cam", "CAM"),
    ("telesync", "Telesync"),
    ("telecine", "Telecine"),
    ("workprint", "Workprint"),
    ("dvdscr", "Screener"),
    ("regional", "Regional"),
    ("tv", "TV"),
    ("dvd", "DVD"),
    ("web_dl", "WEB-DL"),
    ("web_rip", "WEBRip"),
    ("bluray", "Blu-ray"),
    ("remux", "Remux"),
    ("br_disk", "BR-DISK"),
    ("raw_hd", "Raw-HD"),
];
const RESOLUTIONS: &[(&str, &str)] = &[
    ("360p", "360p"),
    ("480p", "480p"),
    ("540p", "540p"),
    ("576p", "576p"),
    ("720p", "720p"),
    ("1080p", "1080p"),
    ("2160p", "2160p"),
];
const MODIFIERS: &[(&str, &str)] = &[
    ("none", "None"),
    ("regional", "Regional"),
    ("screener", "Screener"),
    ("raw_hd", "Raw-HD"),
    ("br_disk", "BR-DISK"),
    ("remux", "Remux"),
];
const FLAGS: &[(&str, &str)] = &[
    ("freeleech", "Freeleech"),
    ("halfleech", "Halfleech"),
    ("double_upload", "Double upload"),
    ("freeleech75", "Freeleech 75%"),
    ("freeleech25", "Freeleech 25%"),
    ("internal", "Internal"),
    ("scene", "Scene"),
    ("golden", "Golden"),
    ("approved", "Approved"),
    ("nuked", "Nuked"),
];
const RELEASE_TYPES: &[(&str, &str)] = &[
    ("single_episode", "Single episode"),
    ("multi_episode", "Multi-episode"),
    ("season_pack", "Season pack"),
];

/// One editable condition. Values are kept as strings and converted on save.
#[derive(Clone, Copy)]
struct SpecRow {
    id: u32,
    name: RwSignal<String>,
    kind: RwSignal<String>,
    value: RwSignal<String>,
    /// Max for size/year; "true"/"false" for the language `except` flag.
    value2: RwSignal<String>,
    negate: RwSignal<bool>,
    required: RwSignal<bool>,
}

fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl SpecRow {
    fn new(id: u32) -> Self {
        Self {
            id,
            name: RwSignal::new(String::new()),
            kind: RwSignal::new("release_title".into()),
            value: RwSignal::new(String::new()),
            value2: RwSignal::new(String::new()),
            negate: RwSignal::new(false),
            required: RwSignal::new(false),
        }
    }

    fn from_spec(id: u32, s: &FormatSpecification) -> Self {
        let row = Self::new(id);
        let v = serde_json::to_value(s).unwrap_or(Value::Null);
        let kind = str_of(&v["type"]);
        row.name.set(s.name.clone());
        row.negate.set(s.negate);
        row.required.set(s.required);
        match kind.as_str() {
            "size" => {
                row.value.set(str_of(&v["min_gb"]));
                row.value2.set(str_of(&v["max_gb"]));
            }
            "year" => {
                row.value.set(str_of(&v["min"]));
                row.value2.set(str_of(&v["max"]));
            }
            "language" => {
                row.value.set(str_of(&v["value"]));
                row.value2.set(str_of(&v["except"]));
            }
            _ => row.value.set(str_of(&v["value"])),
        }
        row.kind.set(kind);
        row
    }

    /// Builds the specification (via its JSON shape); `Err` describes what's missing.
    fn to_spec(self) -> Result<FormatSpecification, String> {
        let kind = self.kind.get_untracked();
        let value = self.value.get_untracked().trim().to_string();
        let value2 = self.value2.get_untracked().trim().to_string();
        let name = self.name.get_untracked().trim().to_string();
        let label = if name.is_empty() {
            kind.clone()
        } else {
            name.clone()
        };
        let num = |s: &str, what: &str| -> Result<f64, String> {
            s.parse::<f64>()
                .map_err(|_| format!("{label}: {what} must be a number"))
        };
        let mut v = json!({
            "name": if name.is_empty() { kind.clone() } else { name },
            "negate": self.negate.get_untracked(),
            "required": self.required.get_untracked(),
            "type": kind,
        });
        match kind.as_str() {
            "size" => {
                v["min_gb"] = json!(num(&value, "minimum")?);
                v["max_gb"] = json!(num(&value2, "maximum")?);
            }
            "year" => {
                v["min"] = json!(num(&value, "minimum")? as u16);
                v["max"] = json!(num(&value2, "maximum")? as u16);
            }
            "language" => {
                v["value"] = if value.is_empty() {
                    Value::Null
                } else {
                    json!(value)
                };
                v["except"] = json!(value2 == "true");
            }
            _ => {
                if value.is_empty() {
                    return Err(format!("{label}: a value is required"));
                }
                v["value"] = json!(value);
            }
        }
        serde_json::from_value(v).map_err(|e| format!("{label}: {e}"))
    }
}

/// Human summary of a condition for the list view.
fn describe(s: &FormatSpecification) -> String {
    let v = serde_json::to_value(s).unwrap_or(Value::Null);
    let kind = str_of(&v["type"]);
    let label = KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(kind.as_str(), |(_, l)| *l);
    let label = label.split(" (").next().unwrap_or(label);
    let detail = match kind.as_str() {
        "size" => format!("{}–{} GB", str_of(&v["min_gb"]), str_of(&v["max_gb"])),
        "year" => format!("{}–{}", str_of(&v["min"]), str_of(&v["max"])),
        "language" => {
            let l = str_of(&v["value"]);
            let l = if l.is_empty() { "Any".to_string() } else { l };
            if v["except"] == true {
                format!("except {l}")
            } else {
                l
            }
        }
        _ => str_of(&v["value"]),
    };
    format!(
        "{}{label}: {detail}{}",
        if s.negate { "NOT " } else { "" },
        if s.required { " (required)" } else { "" }
    )
}

#[component]
pub fn CustomFormatsPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let formats = LocalResource::new(|| api::get::<Vec<CustomFormat>>("/customformats"));

    let editor_open = RwSignal::new(false);
    let editing_id = RwSignal::new(None::<i64>);
    let name = RwSignal::new(String::new());
    let include_in_rename = RwSignal::new(false);
    let rows = RwSignal::new(Vec::<SpecRow>::new());
    let next_id = StoredValue::new(0u32);
    let saving = RwSignal::new(false);

    let import_open = RwSignal::new(false);
    let import_text = RwSignal::new(String::new());
    let export_open = RwSignal::new(false);
    let export_text = RwSignal::new(String::new());
    let delete_target = RwSignal::new(None::<CustomFormat>);
    let delete_open = RwSignal::new(false);
    let test_title = RwSignal::new(String::new());

    let fresh_id = move || {
        let id = next_id.get_value();
        next_id.set_value(id + 1);
        id
    };

    let open_editor = move |f: Option<CustomFormat>| {
        match f {
            Some(f) => {
                editing_id.set(Some(f.id));
                name.set(f.name.clone());
                include_in_rename.set(f.include_in_rename);
                rows.set(
                    f.specifications
                        .iter()
                        .map(|s| SpecRow::from_spec(fresh_id(), s))
                        .collect(),
                );
            }
            None => {
                editing_id.set(None);
                name.set(String::new());
                include_in_rename.set(false);
                rows.set(vec![SpecRow::new(fresh_id())]);
            }
        }
        editor_open.set(true);
    };

    // The format being edited, if every row is valid.
    let draft = move || -> Result<CustomFormat, String> {
        let specifications = rows
            .get()
            .into_iter()
            .map(SpecRow::to_spec)
            .collect::<Result<Vec<_>, _>>()?;
        let f = CustomFormat {
            id: editing_id.get().unwrap_or(0),
            name: name.get().trim().to_string(),
            include_in_rename: include_in_rename.get(),
            specifications,
        };
        f.validate().map_err(|e| e.to_string())?;
        Ok(f)
    };

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let f = match draft() {
            Ok(f) => f,
            Err(msg) => {
                toasts.error(msg);
                return;
            }
        };
        saving.set(true);
        spawn_local(async move {
            let res = match editing_id.get_untracked() {
                Some(id) => api::put::<_, CustomFormat>(&format!("/customformats/{id}"), &f).await,
                None => api::post::<_, CustomFormat>("/customformats", &f).await,
            };
            match res {
                Ok(saved) => {
                    toasts.success(format!("Saved custom format “{}”.", saved.name));
                    editor_open.set(false);
                    formats.refetch();
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    let do_import = move |_| {
        let text = import_text.get_untracked();
        let body: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                toasts.error(format!("That isn't valid JSON: {e}"));
                return;
            }
        };
        spawn_local(async move {
            match api::post::<_, Vec<ImportedFormatDto>>("/customformats/import", &body).await {
                Ok(list) => {
                    let names: Vec<String> = list.iter().map(|i| i.format.name.clone()).collect();
                    toasts.success(format!("Imported {}.", names.join(", ")));
                    import_open.set(false);
                    import_text.set(String::new());
                    formats.refetch();
                }
                Err(e) => toasts.error(format!("Import failed: {e}")),
            }
        });
    };

    let export = move |id: i64| {
        spawn_local(async move {
            match api::get::<Value>(&format!("/customformats/{id}/export")).await {
                Ok(v) => {
                    export_text.set(serde_json::to_string_pretty(&v).unwrap_or_default());
                    export_open.set(true);
                }
                Err(e) => toasts.error(format!("Export failed: {e}")),
            }
        });
    };

    let confirm_delete = Callback::new(move |()| {
        let Some(f) = delete_target.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/customformats/{}", f.id)).await {
                Ok(()) => {
                    toasts.success(format!("Deleted “{}”.", f.name));
                    formats.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    // Live test: which saved formats match the title typed in the test box.
    let test_matches = move || -> Option<(String, Vec<i64>)> {
        let title = test_title.get();
        let title = title.trim();
        if title.is_empty() {
            return None;
        }
        let parsed = reelix_parser::parse(title);
        let summary = format!(
            "{} · {}",
            parsed.quality.quality.name(),
            if parsed.title.is_empty() {
                "?".into()
            } else {
                parsed.title.clone()
            }
        );
        let ids = formats
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .iter()
            .filter(|f| {
                let mut input = FormatInput::from_parsed(&parsed);
                input.title = title;
                f.matches(&input)
            })
            .map(|f| f.id)
            .collect();
        Some((summary, ids))
    };

    let list = move || {
        match formats.get() {
        None => view! { <Skeleton lines=4 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| formats.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState
                icon="sparkle"
                title="No custom formats yet"
                description="Create one, or import TRaSH-guides JSON to score HDR, audio, release groups and more."
            />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <div class="cf-grid">
                {list
                    .into_iter()
                    .map(|f| {
                        let id = f.id;
                        let edit_target = f.clone();
                        let del_target = f.clone();
                        let matched = move || test_matches().is_some_and(|(_, ids)| ids.contains(&id));
                        view! {
                            <article class="cf-card" class:matched=matched>
                                <header class="cf-card-head">
                                    <h3 class="cf-name">{f.name.clone()}</h3>
                                    <Show when=matched>
                                        <span class="badge tone-success">"Matches"</span>
                                    </Show>
                                </header>
                                <div class="chip-list">
                                    {f.specifications.iter().map(|s| view! { <span class="chip">{describe(s)}</span> }).collect_view()}
                                </div>
                                <div class="cf-actions">
                                    <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| open_editor(Some(edit_target.clone()))>
                                        "Edit"
                                    </button>
                                    <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| export(id)>
                                        <Icon name="download" />
                                        "Export"
                                    </button>
                                    <button
                                        type="button"
                                        class="btn btn-sm btn-ghost btn-danger-text"
                                        aria-label=format!("Delete {}", f.name)
                                        on:click=move |_| {
                                            delete_target.set(Some(del_target.clone()));
                                            delete_open.set(true);
                                        }
                                    >
                                        <Icon name="trash" />
                                    </button>
                                </div>
                            </article>
                        }
                    })
                    .collect_view()}
            </div>
        }
        .into_any(),
    }
    };

    // Live check of the draft in the editor against its own test title.
    let editor_test = RwSignal::new(String::new());
    let editor_result = move || {
        let t = editor_test.get();
        if t.trim().is_empty() {
            return None;
        }
        Some(match draft() {
            Err(e) => (false, e),
            Ok(f) => {
                let parsed = reelix_parser::parse(t.trim());
                let mut input = FormatInput::from_parsed(&parsed);
                let title = t.trim().to_string();
                input.title = &title;
                if f.matches(&input) {
                    (true, "This release matches the format.".into())
                } else {
                    (false, "This release does not match.".into())
                }
            }
        })
    };

    view! {
        <PageHeader title="Custom Formats" subtitle="Conditions on a release that profiles can score.">
            <button type="button" class="btn btn-secondary" on:click=move |_| import_open.set(true)>
                <Icon name="download" />
                "Import JSON"
            </button>
            <button type="button" class="btn btn-primary" on:click=move |_| open_editor(None)>
                <Icon name="plus" />
                "New format"
            </button>
        </PageHeader>
        <div class="stack">
            <Card title="Test a release name" subtitle="Matching formats are highlighted below.">
                <input
                    class="input"
                    type="text"
                    placeholder="Movie.Name.2024.2160p.WEB-DL.DDP5.1.Atmos.DV.HDR.H.265-GROUP"
                    aria-label="Release name to test"
                    prop:value=move || test_title.get()
                    on:input=move |e| test_title.set(event_target_value(&e))
                />
                {move || test_matches().map(|(summary, ids)| view! {
                    <p class="field-hint">{format!("Parsed as {summary} — {} format(s) match.", ids.len())}</p>
                })}
            </Card>
            {list}
        </div>

        <Modal open=editor_open title="Custom format">
            <form class="form" on:submit=save>
                <div class="field-row">
                    <div class="field">
                        <label for="cf-name">"Name"</label>
                        <input
                            id="cf-name"
                            class="input"
                            type="text"
                            required
                            maxlength="100"
                            prop:value=move || name.get()
                            on:input=move |e| name.set(event_target_value(&e))
                        />
                    </div>
                    <div class="field field-check">
                        <label class="checkbox">
                            <input
                                type="checkbox"
                                prop:checked=move || include_in_rename.get()
                                on:change=move |e| include_in_rename.set(event_target_checked(&e))
                            />
                            "Include in {Custom Formats} renaming token"
                        </label>
                    </div>
                </div>
                <p class="field-hint">
                    "Conditions of the same type: every required one must match and at least one of the others. Different types must all pass."
                </p>
                <div class="cond-list">
                    <For each=move || rows.get() key=|r| r.id let:row>
                        <ConditionRow row=row on_remove=Callback::new(move |id: u32| rows.update(|r| r.retain(|x| x.id != id))) />
                    </For>
                </div>
                <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| rows.update(|r| r.push(SpecRow::new(fresh_id())))>
                    <Icon name="plus" />
                    "Add condition"
                </button>
                <div class="field">
                    <label for="cf-test">"Test this format"</label>
                    <input
                        id="cf-test"
                        class="input"
                        type="text"
                        placeholder="Paste a release name"
                        prop:value=move || editor_test.get()
                        on:input=move |e| editor_test.set(event_target_value(&e))
                    />
                    {move || editor_result().map(|(ok, msg)| view! {
                        <p class="field-hint" class:text-success=ok class:text-danger={!ok}>{msg}</p>
                    })}
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| editor_open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                </div>
            </form>
        </Modal>

        <Modal open=import_open title="Import custom formats">
            <div class="form">
                <div class="field">
                    <label for="cf-import">"TRaSH-guides or Radarr/Sonarr export JSON"</label>
                    <textarea
                        id="cf-import"
                        class="input textarea mono"
                        rows="12"
                        placeholder="{ \"name\": \"…\", \"specifications\": [ … ] }  — or an array of formats"
                        prop:value=move || import_text.get()
                        on:input=move |e| import_text.set(event_target_value(&e))
                    ></textarea>
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| import_open.set(false)>"Cancel"</button>
                    <button type="button" class="btn btn-primary" disabled=move || import_text.with(|t| t.trim().is_empty()) on:click=do_import>
                        "Import"
                    </button>
                </div>
            </div>
        </Modal>

        <Modal open=export_open title="Export (TRaSH JSON)">
            <div class="form">
                <textarea class="input textarea mono" rows="14" readonly prop:value=move || export_text.get()></textarea>
                <div class="modal-footer">
                    <CopyButton text=Signal::derive(move || export_text.get()) />
                    <button type="button" class="btn btn-primary" on:click=move |_| export_open.set(false)>"Done"</button>
                </div>
            </div>
        </Modal>

        <ConfirmDialog
            open=delete_open
            title="Delete custom format?"
            message=Signal::derive(move || delete_target.with(|f| f.as_ref().map(|f| format!("“{}” and its scores in every profile will be removed.", f.name)).unwrap_or_default()))
            confirm_label="Delete"
            on_confirm=confirm_delete
        />
    }
}

fn options(list: &'static [(&'static str, &'static str)], current: RwSignal<String>) -> impl IntoView {
    list.iter()
        .map(|(v, l)| view! { <option value=*v selected=move || current.get() == *v>{*l}</option> })
        .collect_view()
}

#[component]
fn ConditionRow(row: SpecRow, on_remove: Callback<u32>) -> impl IntoView {
    let editor = move || {
        let kind = row.kind.get();
        // Pick a sensible default when switching to an enum-valued kind.
        let first = |list: &[(&str, &str)]| {
            let cur = row.value.get_untracked();
            if !list.iter().any(|(v, _)| *v == cur) {
                row.value.set(list[0].0.to_string());
            }
        };
        let select = |list: &'static [(&'static str, &'static str)]| {
            first(list);
            view! {
                <select class="input input-sm" aria-label="Value" on:change=move |e| row.value.set(event_target_value(&e))>
                    {options(list, row.value)}
                </select>
            }
            .into_any()
        };
        match kind.as_str() {
            "source" => select(SOURCES),
            "resolution" => select(RESOLUTIONS),
            "quality_modifier" => select(MODIFIERS),
            "indexer_flag" => select(FLAGS),
            "release_type" => select(RELEASE_TYPES),
            "language" => view! {
                <select class="input input-sm" aria-label="Language" on:change=move |e| row.value.set(event_target_value(&e))>
                    <option value="" selected=move || row.value.get().is_empty()>"Any"</option>
                    {Language::ALL
                        .iter()
                        .filter(|l| **l != Language::Unknown)
                        .map(|l| {
                            let n = l.name();
                            view! { <option value=n selected=move || row.value.get() == n>{n}</option> }
                        })
                        .collect_view()}
                </select>
                <label class="checkbox">
                    <input
                        type="checkbox"
                        prop:checked=move || row.value2.get() == "true"
                        on:change=move |e| row.value2.set(event_target_checked(&e).to_string())
                    />
                    "Except"
                </label>
            }
            .into_any(),
            "size" | "year" => {
                let (a, b) = if kind == "size" { ("Min GB", "Max GB") } else { ("From", "To") };
                view! {
                    <input class="input input-sm" type="number" step="any" placeholder=a aria-label=a
                        prop:value=move || row.value.get() on:input=move |e| row.value.set(event_target_value(&e)) />
                    <input class="input input-sm" type="number" step="any" placeholder=b aria-label=b
                        prop:value=move || row.value2.get() on:input=move |e| row.value2.set(event_target_value(&e)) />
                }
                .into_any()
            }
            _ => view! {
                <input
                    class="input input-sm mono"
                    type="text"
                    placeholder="regular expression, e.g. \\bDV\\b"
                    aria-label="Regular expression"
                    prop:value=move || row.value.get()
                    on:input=move |e| row.value.set(event_target_value(&e))
                />
            }
            .into_any(),
        }
    };
    view! {
        <div class="cond-row">
            <input
                class="input input-sm cond-name"
                type="text"
                placeholder="Condition name"
                aria-label="Condition name"
                prop:value=move || row.name.get()
                on:input=move |e| row.name.set(event_target_value(&e))
            />
            <select class="input input-sm cond-kind" aria-label="Condition type" on:change=move |e| row.kind.set(event_target_value(&e))>
                {options(KINDS, row.kind)}
            </select>
            <div class="cond-value">{editor}</div>
            <div class="cond-flags">
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || row.negate.get() on:change=move |e| row.negate.set(event_target_checked(&e)) />
                    "Negate"
                </label>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || row.required.get() on:change=move |e| row.required.set(event_target_checked(&e)) />
                    "Required"
                </label>
            </div>
            <button type="button" class="icon-btn icon-btn-sm cond-remove" aria-label="Remove condition" on:click=move |_| on_remove.run(row.id)>
                <Icon name="close" />
            </button>
        </div>
    }
}
