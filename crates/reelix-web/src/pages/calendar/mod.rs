//! `/calendar` — upcoming episodes and movie releases (month / week / agenda), with an iCal link.

use chrono::{Datelike, Duration, Months, NaiveDate, Utc, Weekday};
use leptos::prelude::*;
use leptos_router::components::A;
use reelix_api_types::Permissions;
use reelix_api_types::library::{CalendarEntryDto, CalendarKind, CalendarStatus, ReleaseKind};
use reelix_api_types::system::FirstDayOfWeek;

use crate::api;
use crate::components::library_common::{Segmented, on_entity_changed, pref_get, pref_set};
use crate::components::library_forms::require_any;
use crate::components::{Card, CopyButton, EmptyState, ErrorBox, Icon, PageHeader, Skeleton};
use crate::state::AuthState;

fn status_class(s: CalendarStatus) -> &'static str {
    match s {
        CalendarStatus::Downloaded => "tone-success",
        CalendarStatus::Missing => "tone-danger",
        CalendarStatus::Upcoming => "tone-info",
        CalendarStatus::Unmonitored => "tone-neutral",
    }
}

fn status_label(s: CalendarStatus) -> &'static str {
    match s {
        CalendarStatus::Downloaded => "Downloaded",
        CalendarStatus::Missing => "Missing",
        CalendarStatus::Upcoming => "Upcoming",
        CalendarStatus::Unmonitored => "Unmonitored",
    }
}

fn release_label(k: ReleaseKind) -> &'static str {
    match k {
        ReleaseKind::Cinema => "In cinemas",
        ReleaseKind::Digital => "Digital release",
        ReleaseKind::Physical => "Physical release",
        ReleaseKind::Airing => "Airs",
    }
}

fn entry_href(e: &CalendarEntryDto) -> String {
    match (e.movie_id, e.series_id) {
        (Some(m), _) => format!("/movies/{m}"),
        (_, Some(s)) => format!("/series/{s}"),
        _ => "/calendar".into(),
    }
}

/// `(main line, secondary line)` for an entry.
fn entry_text(e: &CalendarEntryDto) -> (String, String) {
    match e.kind {
        CalendarKind::Episode => {
            let code = match (e.season_number, e.episode_number) {
                (Some(s), Some(n)) => format!("S{s:02}E{n:02}"),
                _ => String::new(),
            };
            let sub = [Some(code), e.episode_title.clone()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            (e.title.clone(), sub)
        }
        CalendarKind::Movie => (e.title.clone(), release_label(e.release_kind).to_string()),
    }
}

#[component]
fn Entry(entry: CalendarEntryDto, #[prop(optional)] full: bool) -> impl IntoView {
    let (main, sub) = entry_text(&entry);
    let tip = format!(
        "{main} — {sub} ({}){}",
        status_label(entry.status),
        entry
            .quality
            .as_ref()
            .map(|q| format!(" · {q}"))
            .unwrap_or_default()
    );
    let class = format!("cal-entry {}", status_class(entry.status));
    // The colour shows status visually; say it for screen readers too.
    let status_text = format!(" ({})", status_label(entry.status));
    let content = if full {
        view! {
            <strong>{main}</strong>
            " "
            <span class="cal-sub">{sub}</span>
            {entry.network.clone().map(|n| view! { <span class="cal-sub">{format!(" · {n}")}</span> })}
            {entry.quality.clone().map(|q| view! { <span class="badge tone-success">{q}</span> })}
            <span class="visually-hidden">{status_text}</span>
        }
        .into_any()
    } else {
        view! {
            {main}
            {(!sub.is_empty()).then(|| view! { <span class="cal-sub">{format!(" {sub}")}</span> })}
            <span class="visually-hidden">{status_text}</span>
        }
        .into_any()
    };
    // Library pages are for managers; calendar-only viewers get plain entries.
    if AuthState::expect().has(Permissions::MANAGE_LIBRARY) {
        view! { <A href=entry_href(&entry) attr:class=class attr:title=tip>{content}</A> }.into_any()
    } else {
        view! { <span class=class title=tip>{content}</span> }.into_any()
    }
}

fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

/// First day of the week containing `d`, per the user's UI settings.
fn week_start(d: NaiveDate) -> NaiveDate {
    let offset = match crate::format::prefs().first_day_of_week {
        FirstDayOfWeek::Monday => d.weekday().num_days_from_monday(),
        FirstDayOfWeek::Sunday => d.weekday().num_days_from_sunday(),
    };
    d - Duration::days(i64::from(offset))
}

#[component]
pub fn CalendarPage() -> impl IntoView {
    require_any(Permissions::MANAGE_LIBRARY | Permissions::VIEW_CALENDAR);
    let today = Utc::now().date_naive();
    let view_mode = RwSignal::new(
        pref_get("calendar.view").unwrap_or_else(|| crate::format::prefs().calendar_view.as_str().into()),
    );
    let anchor = RwSignal::new(today);
    let show_unmonitored = RwSignal::new(pref_get("calendar.unmonitored").as_deref() == Some("1"));
    let kinds = RwSignal::new("all".to_string());

    // Remember choices made here; until then the UI settings' default view applies.
    Effect::new(move |prev: Option<()>| {
        let view = view_mode.get();
        let unmonitored = show_unmonitored.get();
        if prev.is_some() {
            pref_set("calendar.view", &view);
            pref_set("calendar.unmonitored", if unmonitored { "1" } else { "0" });
        }
    });

    // Visible date range for the current view.
    let range = Memo::new(move |_| {
        let a = anchor.get();
        match view_mode.get().as_str() {
            "week" => {
                let s = week_start(a);
                (s, s + Duration::days(6))
            }
            "agenda" => (a, a + Duration::days(29)),
            _ => {
                let s = week_start(month_start(a));
                let last = month_start(a) + Months::new(1) - Duration::days(1);
                let e = week_start(last) + Duration::days(6);
                (s, e)
            }
        }
    });

    let entries = LocalResource::new(move || {
        let (s, e) = range.get();
        let unmon = show_unmonitored.get();
        async move {
            api::get::<Vec<CalendarEntryDto>>(&format!(
                "/calendar?start={}&end={}&unmonitored={unmon}",
                s.format("%Y-%m-%d"),
                e.format("%Y-%m-%d")
            ))
            .await
        }
    });
    on_entity_changed(
        &["movie", "series", "episode", "movie_file", "episode_file"],
        true,
        move || entries.refetch(),
    );

    let filtered = move || {
        let k = kinds.get();
        entries.get().map(|r| {
            r.map(|list| {
                list.into_iter()
                    .filter(|e| match k.as_str() {
                        "movies" => e.kind == CalendarKind::Movie,
                        "episodes" => e.kind == CalendarKind::Episode,
                        _ => true,
                    })
                    .collect::<Vec<_>>()
            })
        })
    };

    let step = move |dir: i32| {
        anchor.update(|a| {
            *a = match view_mode.get_untracked().as_str() {
                "week" => *a + Duration::days(7 * i64::from(dir)),
                "agenda" => *a + Duration::days(30 * i64::from(dir)),
                _ => {
                    let m = month_start(*a);
                    if dir > 0 {
                        m + Months::new(1)
                    } else {
                        m - Months::new(1)
                    }
                }
            }
        });
    };

    let title = move || {
        let (s, e) = range.get();
        match view_mode.get().as_str() {
            "month" => anchor.get().format("%B %Y").to_string(),
            _ => format!("{} – {}", s.format("%b %-d"), e.format("%b %-d, %Y")),
        }
    };

    let grid = move || {
        let list = match filtered() {
            None => return view! { <Skeleton lines=8 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| entries.refetch()) /> }
                    .into_any();
            }
            Some(Ok(l)) => l,
        };
        let (start, end) = range.get();
        let mode = view_mode.get();
        if mode == "agenda" {
            if list.is_empty() {
                return view! {
                    <div class="card"><EmptyState icon="calendar" title="Nothing scheduled" description="No releases or airings in the next 30 days." /></div>
                }
                .into_any();
            }
            let mut days: Vec<NaiveDate> = list.iter().map(|e| e.date).collect();
            days.sort();
            days.dedup();
            return view! {
                <div class="card">
                    <div class="card-body">
                        {days.into_iter().map(|d| {
                            let items: Vec<_> = list.iter().filter(|e| e.date == d).cloned().collect();
                            view! {
                                <div class="agenda-day" class:today=d == today>
                                    <div class="agenda-date">{d.format("%A, %B %-d").to_string()}{(d == today).then_some(" · Today")}</div>
                                    <div class="agenda-list">
                                        {items.into_iter().map(|e| view! { <Entry entry=e full=true /> }).collect_view()}
                                    </div>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                </div>
            }
            .into_any();
        }
        let month = anchor.get().month();
        let mut days = Vec::new();
        let mut d = start;
        while d <= end {
            days.push(d);
            d += Duration::days(1);
        }
        let first = match crate::format::prefs().first_day_of_week {
            FirstDayOfWeek::Monday => Weekday::Mon,
            FirstDayOfWeek::Sunday => Weekday::Sun,
        };
        let dows: Vec<Weekday> = std::iter::successors(Some(first), |w| Some(w.succ()))
            .take(7)
            .collect();
        view! {
            <div class="cal-grid" class:cal-week=mode == "week">
                {dows.into_iter().map(|w| view! { <div class="cal-dow" aria-hidden="true">{format!("{w}")}</div> }).collect_view()}
                {days.into_iter().map(|day| {
                    let items: Vec<_> = list.iter().filter(|e| e.date == day).cloned().collect();
                    let other = mode == "month" && day.month() != month;
                    view! {
                        <section class="cal-day" class:other=other class:today=day == today>
                            <h3 class="visually-hidden">{day.format("%A %B %-d").to_string()}</h3>
                            <span class="cal-date" aria-hidden="true">{day.day()}</span>
                            {items.into_iter().map(|e| view! { <Entry entry=e /> }).collect_view()}
                        </section>
                    }
                }).collect_view()}
            </div>
        }
        .into_any()
    };

    let origin = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();
    // The full key is only returned when it is created, so the user either creates a
    // dedicated calendar key here (shown once) or pastes an existing one.
    let ical_key = RwSignal::new(String::new());
    let ical_unmonitored = RwSignal::new(false);
    let creating_key = RwSignal::new(false);
    let key_error = RwSignal::new(None::<String>);
    let feed = api::api_url("/calendar.ics");
    let ical = Signal::derive(move || {
        let key = ical_key.get();
        let key = key.trim();
        format!(
            "{origin}{feed}?apikey={}&future_days=60{}",
            if key.is_empty() { "API_KEY" } else { key },
            if ical_unmonitored.get() {
                "&unmonitored=true"
            } else {
                ""
            }
        )
    });
    let create_key = move |_| {
        creating_key.set(true);
        key_error.set(None);
        leptos::task::spawn_local(async move {
            let req = reelix_api_types::CreateApiKeyRequest {
                name: "Calendar feed".into(),
            };
            match api::post::<_, reelix_api_types::CreatedApiKey>("/auth/apikeys", &req).await {
                Ok(k) => ical_key.set(k.key),
                Err(e) => key_error.set(Some(e.to_string())),
            }
            creating_key.set(false);
        });
    };

    let period_name = move || match view_mode.get().as_str() {
        "week" => "week",
        "agenda" => "period",
        _ => "month",
    };
    view! {
        <PageHeader title="Calendar" subtitle="Upcoming episodes and movie releases." />
        <div class="cal-toolbar">
            <button type="button" class="btn btn-sm btn-secondary" aria-label=move || format!("Previous {}", period_name()) on:click=move |_| step(-1)>"‹"</button>
            <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| anchor.set(today)>"Today"</button>
            <button type="button" class="btn btn-sm btn-secondary" aria-label=move || format!("Next {}", period_name()) on:click=move |_| step(1)>"›"</button>
            <h2 class="cal-title" aria-live="polite">{title}</h2>
            <Segmented value=view_mode label="Calendar view" options=vec![("month", "Month"), ("week", "Week"), ("agenda", "Agenda")] />
            <Segmented value=kinds label="Show" options=vec![("all", "All"), ("movies", "Movies"), ("episodes", "Episodes")] />
            <label class="checkbox">
                <input type="checkbox" prop:checked=move || show_unmonitored.get()
                    on:change=move |e| show_unmonitored.set(event_target_checked(&e)) />
                "Unmonitored"
            </label>
        </div>
        {grid}
        <div class="cal-legend" aria-label="Legend">
            {[CalendarStatus::Downloaded, CalendarStatus::Missing, CalendarStatus::Upcoming, CalendarStatus::Unmonitored]
                .into_iter()
                .map(|s| view! { <span class=status_class(s)><i></i>{status_label(s)}</span> })
                .collect_view()}
        </div>
        <div class="stack cal-ical">
            <Card title="iCal feed" subtitle="Subscribe from Google Calendar, Outlook or Apple Calendar.">
                <div class="lib-toolbar ical-key">
                    <input class="input grow mono" type="text" placeholder="Paste an API key" aria-label="API key for the feed"
                        prop:value=move || ical_key.get() on:input=move |e| ical_key.set(event_target_value(&e)) />
                    <button type="button" class="btn btn-sm btn-secondary" disabled=move || creating_key.get() on:click=create_key>
                        <Icon name="key" />
                        {move || if creating_key.get() { "Creating…" } else { "Create calendar key" }}
                    </button>
                </div>
                {move || key_error.get().map(|e| view! { <p class="field-error" role="alert">{e}</p> })}
                <div class="lib-toolbar">
                    <input class="input grow mono" type="text" readonly aria-label="iCal URL" prop:value=move || ical.get() />
                    <CopyButton text=ical label="Copy link" />
                </div>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || ical_unmonitored.get()
                        on:change=move |e| ical_unmonitored.set(event_target_checked(&e)) />
                    "Include unmonitored items"
                </label>
                <p class="field-hint">
                    <Icon name="info" />
                    " A new key is shown only once — copy the link now. Manage keys under "
                    <A href="/settings/general">"Settings → General"</A>
                    "."
                </p>
            </Card>
        </div>
    }
}
