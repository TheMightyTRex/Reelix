//! Human-friendly formatting of times and durations.

use chrono::{DateTime, Local, Utc};

/// `2026-10-28 14:03` in the browser's time zone.
pub fn datetime(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

/// `14:03:12` in the browser's time zone.
pub fn time(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Local).format("%H:%M:%S").to_string()
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

/// `in 5m`, `3h ago`, `just now`.
pub fn relative(dt: DateTime<Utc>) -> String {
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
