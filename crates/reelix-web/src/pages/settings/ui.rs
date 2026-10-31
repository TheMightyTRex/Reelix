//! `/settings/ui` — the signed-in user's date, time, calendar and library display preferences.

use chrono::{Duration, Utc};
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::system::{
    CalendarView, DateFormat, FirstDayOfWeek, LibraryView, TimeFormat, UiSettings,
};

use crate::api;
use crate::components::{Card, ErrorBox, PageHeader, Skeleton};
use crate::format;
use crate::state::Toasts;

#[component]
pub fn UiSettingsPage() -> impl IntoView {
    let settings = LocalResource::new(|| api::get::<UiSettings>("/settings/ui"));
    view! {
        <PageHeader title="UI" subtitle="How dates, the calendar and libraries look for you. These settings follow your account." />
        {move || match settings.get() {
            None => view! { <Skeleton lines=8 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) /> }.into_any(),
            Some(Ok(s)) => view! { <UiForm s=s /> }.into_any(),
        }}
    }
}

/// A `<select>` over `options`, bound to `value`.
#[component]
fn Choice<T: Copy + PartialEq + Send + Sync + 'static>(
    id: &'static str,
    #[prop(into)] label: String,
    value: RwSignal<T>,
    options: Vec<(T, String)>,
) -> impl IntoView {
    let opts = StoredValue::new(options);
    let index = move || {
        opts.with_value(|o| o.iter().position(|(v, _)| *v == value.get()).unwrap_or(0))
            .to_string()
    };
    view! {
        <div class="field">
            <label for=id>{label}</label>
            <select id=id class="input" prop:value=index
                on:change=move |e| {
                    if let Ok(i) = event_target_value(&e).parse::<usize>()
                        && let Some(v) = opts.with_value(|o| o.get(i).map(|(v, _)| *v))
                    {
                        value.set(v);
                    }
                }>
                {opts.with_value(|o| {
                    o.iter()
                        .enumerate()
                        .map(|(i, (_, text))| view! { <option value=i.to_string()>{text.clone()}</option> })
                        .collect_view()
                })}
            </select>
        </div>
    }
}

#[component]
fn UiForm(s: UiSettings) -> impl IntoView {
    let toasts = Toasts::expect();
    let date_format = RwSignal::new(s.date_format);
    let time_format = RwSignal::new(s.time_format);
    let relative = RwSignal::new(s.relative_dates);
    let first_day = RwSignal::new(s.first_day_of_week);
    let calendar_view = RwSignal::new(s.calendar_view);
    let library_view = RwSignal::new(s.library_view);
    let saving = RwSignal::new(false);

    let current = move || UiSettings {
        date_format: date_format.get(),
        time_format: time_format.get(),
        relative_dates: relative.get(),
        first_day_of_week: first_day.get(),
        calendar_view: calendar_view.get(),
        library_view: library_view.get(),
    };

    // Options are labelled with an example rendered in that format.
    let sample = Utc::now();
    let base = format::prefs();
    let date_options = [DateFormat::Iso, DateFormat::Us, DateFormat::Eu, DateFormat::Long]
        .into_iter()
        .map(|d| {
            (
                d,
                format::with_prefs(
                    UiSettings {
                        date_format: d,
                        ..base
                    },
                    || format::date(sample),
                ),
            )
        })
        .collect::<Vec<_>>();
    let time_options = [TimeFormat::H24, TimeFormat::H12]
        .into_iter()
        .map(|t| {
            (
                t,
                format::with_prefs(
                    UiSettings {
                        time_format: t,
                        ..base
                    },
                    || format::time(sample),
                ),
            )
        })
        .collect::<Vec<_>>();

    // Live preview of the unsaved choices.
    let preview = move || {
        format::with_prefs(current(), || {
            let now = Utc::now();
            (
                format::datetime(now),
                format::relative(now - Duration::hours(3)),
                format::date(now + Duration::days(30)),
            )
        })
    };

    let save = move |_| {
        let body = current();
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, UiSettings>("/settings/ui", &body).await {
                Ok(saved) => {
                    format::set_prefs(saved);
                    toasts.success("UI settings saved. Pages you open now use them.");
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    view! {
        <div class="stack">
            <Card title="Dates and times">
                <div class="form">
                    <div class="field-row">
                        <Choice id="ui-date" label="Date format" value=date_format options=date_options />
                        <Choice id="ui-time" label="Time format" value=time_format options=time_options />
                    </div>
                    <label class="checkbox toggle-row">
                        <input type="checkbox" prop:checked=move || relative.get()
                            on:change=move |e| relative.set(event_target_checked(&e)) />
                        <span>
                            <span class="perm-label">"Relative dates"</span>
                            <span class="perm-desc">"Show “3h ago” and “in 2d” instead of the full date where space is short."</span>
                        </span>
                    </label>
                    <div class="preview-box" aria-live="polite">
                        {move || {
                            let (now, ago, later) = preview();
                            view! {
                                <div><span class="muted">"Now: "</span>{now}</div>
                                <div><span class="muted">"Three hours ago: "</span>{ago}</div>
                                <div><span class="muted">"In 30 days: "</span>{later}</div>
                            }
                        }}
                    </div>
                </div>
            </Card>
            <Card title="Calendar">
                <div class="form">
                    <div class="field-row">
                        <Choice id="ui-week" label="First day of the week" value=first_day
                            options=vec![(FirstDayOfWeek::Monday, "Monday".into()), (FirstDayOfWeek::Sunday, "Sunday".into())] />
                        <Choice id="ui-cal" label="Default view" value=calendar_view
                            options=vec![
                                (CalendarView::Month, "Month".into()),
                                (CalendarView::Week, "Week".into()),
                                (CalendarView::Agenda, "Agenda".into()),
                            ] />
                    </div>
                </div>
            </Card>
            <Card title="Libraries" subtitle="Movies, series, authors and artists open in this layout until you pick another one on that page.">
                <div class="form">
                    <Choice id="ui-lib" label="Default layout" value=library_view
                        options=vec![
                            (LibraryView::Posters, "Posters".into()),
                            (LibraryView::Overview, "Overview".into()),
                            (LibraryView::Table, "Table".into()),
                        ] />
                </div>
            </Card>
            <div class="form-actions sticky-actions">
                <button type="button" class="btn btn-primary" on:click=save disabled=move || saving.get()>
                    {move || if saving.get() { "Saving…" } else { "Save changes" }}
                </button>
            </div>
        </div>
    }
}
