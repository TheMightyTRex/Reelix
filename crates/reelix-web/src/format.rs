//! Human-friendly formatting of dates, times and durations, following the user's UI settings.

use std::cell::Cell;

use chrono::{DateTime, Local, NaiveDate, Utc};
use reelix_api_types::system::{DateFormat, TimeFormat, UiSettings};

thread_local! {
    static PREFS: Cell<UiSettings> = Cell::new(UiSettings::default());
}

/// The signed-in user's display preferences (defaults until they load).
pub fn prefs() -> UiSettings {
    PREFS.with(Cell::get)
}

/// Applies new display preferences to everything rendered from now on.
pub fn set_prefs(p: UiSettings) {
    PREFS.with(|c| c.set(p));
}

/// Runs `f` with `p` as the preferences (for previews), then restores the current ones.
pub fn with_prefs<R>(p: UiSettings, f: impl FnOnce() -> R) -> R {
    let saved = PREFS.with(|c| c.replace(p));
    let out = f();
    PREFS.with(|c| c.set(saved));
    out
}

fn date_pattern(f: DateFormat) -> &'static str {
    match f {
        DateFormat::Iso => "%Y-%m-%d",
        DateFormat::Us => "%m/%d/%Y",
        DateFormat::Eu => "%d/%m/%Y",
        DateFormat::Long => "%b %-d, %Y",
    }
}

fn time_pattern(f: TimeFormat, seconds: bool) -> &'static str {
    match (f, seconds) {
        (TimeFormat::H24, false) => "%H:%M",
        (TimeFormat::H24, true) => "%H:%M:%S",
        (TimeFormat::H12, false) => "%-I:%M %p",
        (TimeFormat::H12, true) => "%-I:%M:%S %p",
    }
}

/// A calendar date, or a timestamp shown as its date in the browser's time zone.
pub trait AsDate {
    fn local_date(&self) -> NaiveDate;
}

impl AsDate for NaiveDate {
    fn local_date(&self) -> NaiveDate {
        *self
    }
}

impl AsDate for DateTime<Utc> {
    fn local_date(&self) -> NaiveDate {
        self.with_timezone(&Local).date_naive()
    }
}

/// `2026-10-28` (or the user's date format).
pub fn date(d: impl AsDate) -> String {
    d.local_date()
        .format(date_pattern(prefs().date_format))
        .to_string()
}

/// `2026-10-28 14:03` in the browser's time zone (or the user's formats).
pub fn datetime(dt: DateTime<Utc>) -> String {
    let p = prefs();
    let local = dt.with_timezone(&Local);
    format!(
        "{} {}",
        local.format(date_pattern(p.date_format)),
        local.format(time_pattern(p.time_format, false))
    )
}

/// `14:03:12` in the browser's time zone (or the user's time format).
pub fn time(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Local)
        .format(time_pattern(prefs().time_format, true))
        .to_string()
}

/// Compact duration: `45s`, `12m`, `3h 20m`, `2d 4h`.
pub fn duration_secs(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m"),
        (0, _, 0) => format!("{h}h"),
        (0, _, _) => format!("{h}h {m}m"),
        (_, 0, _) => format!("{d}d"),
        _ => format!("{d}d {h}h"),
    }
}

/// Milliseconds as `850 ms` or `2.4 s` (falls back to [`duration_secs`] for long runs).
pub fn duration_ms(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        duration_secs((ms / 1000) as u64)
    }
}

/// `in 5m`, `3h ago`, `just now`; the full date and time when relative dates are turned off.
pub fn relative(dt: DateTime<Utc>) -> String {
    if !prefs().relative_dates {
        return datetime(dt);
    }
    let delta = dt.signed_duration_since(Utc::now()).num_seconds();
    if delta.abs() < 10 {
        "just now".into()
    } else if delta > 0 {
        format!("in {}", duration_secs(delta as u64))
    } else {
        format!("{} ago", duration_secs(delta.unsigned_abs()))
    }
}

/// `Every 6h`, `Every 1d`.
pub fn interval(secs: i64) -> String {
    if secs <= 0 {
        "Manual".into()
    } else {
        format!("Every {}", duration_secs(secs as u64))
    }
}

/// `housekeeping` / `refresh_metadata` → `Housekeeping` / `Refresh metadata`.
pub fn humanize_kind(kind: &str) -> String {
    let text = kind.replace(['_', '-', '.'], " ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
