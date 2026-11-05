//! Parsing helpers shared by the adapters: sizes, dates, settings access.

use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde_json::{Map, Value};

use crate::IndexerError;

/// Parses `"1.5 GB"`, `"700MiB"`, `"1,234.5 MB"`, `"123456789"` into bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().replace(',', "");
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let unit = unit.trim().to_ascii_lowercase();
    let mul: f64 = match unit
        .trim_end_matches("ytes")
        .trim_end_matches('b')
        .trim_end_matches('i')
    {
        "" | "b" => 1.0,
        "k" => 1024.0,
        "m" => 1024f64.powi(2),
        "g" => 1024f64.powi(3),
        "t" => 1024f64.powi(4),
        "p" => 1024f64.powi(5),
        _ => return None,
    };
    Some((n * mul).round() as u64)
}

/// Best-effort date parsing: RFC 2822/3339, unix seconds, common SQL-ish layouts and
/// relative "x ago" strings (relative to `now`).
pub fn parse_date(s: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(d) = DateTime::parse_from_rfc2822(s) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    if s.chars().all(|c| c.is_ascii_digit()) && s.len() >= 9 {
        let n: i64 = s.parse().ok()?;
        let secs = if n > 100_000_000_000 { n / 1000 } else { n };
        return Utc.timestamp_opt(secs, 0).single();
    }
    for f in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%d/%m/%Y %H:%M",
        "%Y/%m/%d %H:%M:%S",
    ] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Some(d.and_utc());
        }
    }
    for f in ["%Y-%m-%d", "%d %b %Y", "%b %d %Y", "%d/%m/%Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return d.and_hms_opt(0, 0, 0).map(|d| d.and_utc());
        }
    }
    parse_time_ago(s, now)
}

/// `"3 hours ago"`, `"1 day, 2 hours"`, `"2w ago"`, `"today"`, `"yesterday"`, `"just now"`.
pub fn parse_time_ago(s: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let l = s.trim().to_ascii_lowercase();
    match l.as_str() {
        "now" | "just now" | "today" => return Some(now),
        "yesterday" => return Some(now - ChronoDuration::days(1)),
        _ => {}
    }
    let re = regex_cache();
    let mut total = ChronoDuration::zero();
    let mut found = false;
    for c in re.captures_iter(&l) {
        let n: f64 = c[1].parse().ok()?;
        let unit = &c[2];
        let secs = match unit {
            u if u.starts_with("mo") => 2_592_000.0,
            u if u.starts_with("mi") || u == "m" => 60.0,
            u if u.starts_with('s') => 1.0,
            u if u.starts_with('h') => 3600.0,
            u if u.starts_with('d') => 86_400.0,
            u if u.starts_with('w') => 604_800.0,
            u if u.starts_with('y') => 31_536_000.0,
            _ => continue,
        };
        // Absurd values ("100000000000 years ago") overflow chrono and would panic: give up.
        total = total.checked_add(&ChronoDuration::try_seconds((n * secs) as i64)?)?;
        found = true;
    }
    if !found {
        return None;
    }
    now.checked_sub_signed(total)
}

fn regex_cache() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(\d+(?:\.\d+)?)\s*(months?|mo|minutes?|mins?|m|seconds?|secs?|s|hours?|hrs?|h|days?|d|weeks?|w|years?|yrs?|y)\b")
            .expect("valid regex")
    })
}

/// Typed accessors on a validated settings object.
pub(crate) trait SettingsExt {
    fn str_opt(&self, k: &str) -> Option<String>;
    fn str_req(&self, k: &str) -> Result<String, IndexerError>;
    fn u32_list(&self, k: &str) -> Vec<u32>;
}

impl SettingsExt for Map<String, Value> {
    fn str_opt(&self, k: &str) -> Option<String> {
        match self.get(k)? {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    fn str_req(&self, k: &str) -> Result<String, IndexerError> {
        self.str_opt(k)
            .ok_or_else(|| IndexerError::Config(format!("{k} is required")))
    }

    fn u32_list(&self, k: &str) -> Vec<u32> {
        let parse = |v: &Value| match v {
            Value::Number(n) => n.as_u64().map(|n| n as u32),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        match self.get(k) {
            Some(Value::Array(a)) => a.iter().filter_map(parse).collect(),
            Some(Value::String(s)) => s.split(',').filter_map(|p| p.trim().parse().ok()).collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_ago_too_far_back_is_none_not_a_panic() {
        let now = Utc::now();
        assert_eq!(parse_time_ago("100000000000 years ago", now), None);
        assert_eq!(parse_time_ago("9999999999999999999 days ago", now), None);
        assert!(parse_time_ago("3 hours ago", now).is_some());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1.5 GB"), Some(1_610_612_736));
        assert_eq!(parse_size("700MiB"), Some(734_003_200));
        assert_eq!(parse_size("1,024 KB"), Some(1_048_576));
        assert_eq!(parse_size("12345"), Some(12345));
        assert_eq!(parse_size("12 bytes"), Some(12));
        assert_eq!(parse_size("lots"), None);
    }

    #[test]
    fn dates() {
        let now = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        assert_eq!(
            parse_date("Tue, 28 Apr 2026 10:00:00 +0000", now)
                .unwrap()
                .to_rfc3339(),
            "2026-04-28T10:00:00+00:00"
        );
        assert_eq!(
            parse_date("2026-04-30 08:30:00", now).unwrap().to_rfc3339(),
            "2026-04-30T08:30:00+00:00"
        );
        assert_eq!(
            parse_date("1777636800", now).unwrap().to_rfc3339(),
            "2026-05-01T12:00:00+00:00"
        );
        assert_eq!(
            parse_date("3 hours ago", now).unwrap(),
            now - ChronoDuration::hours(3)
        );
        assert_eq!(
            parse_date("1 day, 2 hours ago", now).unwrap(),
            now - ChronoDuration::hours(26)
        );
        assert_eq!(
            parse_date("2 weeks", now).unwrap(),
            now - ChronoDuration::weeks(2)
        );
        assert_eq!(
            parse_date("yesterday", now).unwrap(),
            now - ChronoDuration::days(1)
        );
        assert!(parse_date("garbage", now).is_none());
    }
}
