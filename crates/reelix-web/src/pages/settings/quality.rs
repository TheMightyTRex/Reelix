//! `/settings/quality` — quality definitions per family: title and size limits (MB per minute,
//! or MB per book for ebooks).

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::profiles::{QualityDefinition, QualityFamily, SizeBasis};

use super::profiles::{FamilyTabs, family_path};
use crate::api;
use crate::components::{Card, ErrorBox, Icon, PageHeader, Skeleton};
use crate::pages::system::require_admin;
use crate::state::Toasts;

/// Upper bound of the sliders (in the family's unit); larger values can still be typed.
fn slider_max(f: QualityFamily) -> f64 {
    match f {
        QualityFamily::Video => 1000.0,
        QualityFamily::Ebook => 500.0,
        QualityFamily::Audiobook => 20.0,
        QualityFamily::Music => 60.0,
    }
}

/// One editable row; each value has its own signal so typing never re-renders the table.
#[derive(Clone, Copy)]
struct Row {
    def: StoredValue<QualityDefinition>,
    title: RwSignal<String>,
    min: RwSignal<f64>,
    preferred: RwSignal<Option<f64>>,
    max: RwSignal<Option<f64>>,
}

impl Row {
    fn new(d: QualityDefinition) -> Self {
        Self {
            title: RwSignal::new(d.title.clone()),
            min: RwSignal::new(d.min_size),
            preferred: RwSignal::new(d.preferred_size),
            max: RwSignal::new(d.max_size),
            def: StoredValue::new(d),
        }
    }

    fn current(&self) -> QualityDefinition {
        let mut d = self.def.get_value();
        d.title = self.title.get_untracked();
        d.min_size = self.min.get_untracked();
        d.preferred_size = self.preferred.get_untracked();
        d.max_size = self.max.get_untracked();
        d
    }
}

fn parse_opt(s: &str) -> Option<f64> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn fmt_opt(v: Option<f64>) -> String {
    v.map(|v| format!("{v}")).unwrap_or_default()
}

/// Human hint for a size value of `family`: "1.2 GB/h" (video), "144 MB/h" (audiobook),
/// "≈ 108 MB per 45-min album" (music) or "per book" (ebook).
fn size_hint(f: QualityFamily, v: Option<f64>) -> String {
    let Some(v) = v else {
        return "unlimited".into();
    };
    match f {
        QualityFamily::Video => format!("{:.1} GB/h", v * 60.0 / 1024.0),
        QualityFamily::Audiobook => format!("{:.0} MB/h", v * 60.0),
        QualityFamily::Music => format!("≈ {:.0} MB per 45-min album", v * 45.0),
        QualityFamily::Ebook => format!("{v} MB per book"),
    }
}

fn subtitle(f: QualityFamily) -> &'static str {
    match f {
        QualityFamily::Video => {
            "Size limits per minute of runtime. Releases outside these limits are rejected; the preferred size breaks ties."
        }
        QualityFamily::Ebook => {
            "Size limits per book. Releases outside these limits are rejected; collections may be as large as their book count allows."
        }
        QualityFamily::Audiobook => {
            "Size limits per minute of narration, checked when the edition's duration is known."
        }
        QualityFamily::Music => {
            "Size limits per minute of playing time, checked against the album's total track length."
        }
    }
}

#[component]
pub fn QualityPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let family = RwSignal::new(QualityFamily::Video);
    let defs = LocalResource::new(move || {
        let path = family_path("/qualitydefinitions", family.get());
        async move { api::get::<Vec<QualityDefinition>>(&path).await }
    });
    let rows = RwSignal::new(Vec::<Row>::new());
    let saving = RwSignal::new(false);

    Effect::new(move |_| {
        if let Some(Ok(list)) = defs.get() {
            rows.set(list.into_iter().map(Row::new).collect());
        }
    });

    let save = move |_| {
        let payload: Vec<QualityDefinition> = rows.get_untracked().iter().map(Row::current).collect();
        if let Some(bad) = payload.iter().find_map(|d| d.validate().err()) {
            toasts.error(bad);
            return;
        }
        saving.set(true);
        // The rows' own family (not the tab), in case the tab changed while loading.
        let shown = payload
            .first()
            .map_or(family.get_untracked(), |d| d.quality.family());
        let path = family_path("/qualitydefinitions", shown);
        spawn_local(async move {
            match api::put::<_, Vec<QualityDefinition>>(&path, &payload).await {
                Ok(list) => {
                    rows.set(list.into_iter().map(Row::new).collect());
                    toasts.success("Quality definitions saved.");
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    let table = move || {
        match defs.get() {
        None => view! { <Skeleton lines=6 /> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| defs.refetch()) /> }.into_any()
        }
        Some(Ok(_)) => view! {
            <div class="table-wrap">
                <table class="table quality-table">
                    <caption class="visually-hidden">"Quality definitions"</caption>
                    <thead>
                        <tr>
                            <th scope="col">"Quality"</th>
                            <th scope="col">"Title"</th>
                            <th scope="col">{move || {
                                let shown = rows.with(|r| r.first().map(|r| r.def.with_value(|d| d.quality.family())));
                                format!("Min ({})", SizeBasis::of(shown.unwrap_or(QualityFamily::Video)).unit())
                            }}</th>
                            <th scope="col">"Preferred"</th>
                            <th scope="col">"Max"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each=move || rows.get() key=|r| r.def.with_value(|d| d.quality.id()) let:row>
                            <QualityRow row=row />
                        </For>
                    </tbody>
                </table>
            </div>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="Quality" subtitle=subtitle(QualityFamily::Video)>
            <button type="button" class="btn btn-primary" disabled=move || saving.get() on:click=save>
                <Icon name="check" />
                {move || if saving.get() { "Saving…" } else { "Save changes" }}
            </button>
        </PageHeader>
        <FamilyTabs family=family label="Quality family" />
        {move || {
            let f = family.get();
            (f != QualityFamily::Video).then(|| view! { <p class="field-hint">{subtitle(f)}</p> })
        }}
        <Card>{table}</Card>
    }
}

#[component]
fn QualityRow(row: Row) -> impl IntoView {
    let family = row.def.with_value(|d| d.quality.family());
    let max = slider_max(family);
    let step = if max >= 100.0 { 1.0 } else { 0.1 };
    let name = row.def.with_value(|d| d.quality.name().to_string());
    let slider = move |sig: RwSignal<Option<f64>>, label: &'static str| {
        view! {
            <div class="size-cell">
                <input
                    class="input input-sm"
                    type="number"
                    min="0"
                    step="0.1"
                    placeholder="∞"
                    aria-label=format!("{label} for {}", row.def.with_value(|d| d.quality.name()))
                    prop:value=move || fmt_opt(sig.get())
                    on:change=move |e| sig.set(parse_opt(&event_target_value(&e)))
                />
                <input
                    class="range"
                    type="range"
                    min="0"
                    max=max
                    step=step
                    tabindex="-1"
                    aria-hidden="true"
                    prop:value=move || sig.get().unwrap_or(max).min(max).to_string()
                    on:input=move |e| {
                        let v = parse_opt(&event_target_value(&e)).unwrap_or(0.0);
                        sig.set(if v >= max { None } else { Some(v) });
                    }
                />
                <span class="field-hint">{move || size_hint(family, sig.get())}</span>
            </div>
        }
    };
    let min_opt = RwSignal::new(Some(row.min.get_untracked()));
    Effect::new(move |_| row.min.set(min_opt.get().unwrap_or(0.0)));
    view! {
        <tr>
            <td class="cell-strong">{name.clone()}</td>
            <td>
                <input
                    class="input input-sm"
                    type="text"
                    maxlength="64"
                    aria-label=format!("Title for {name}")
                    prop:value=move || row.title.get()
                    on:input=move |e| row.title.set(event_target_value(&e))
                />
            </td>
            <td>{slider(min_opt, "Minimum size")}</td>
            <td>{slider(row.preferred, "Preferred size")}</td>
            <td>{slider(row.max, "Maximum size")}</td>
        </tr>
    }
}
