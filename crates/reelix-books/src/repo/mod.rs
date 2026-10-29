//! Book repositories (runtime `sqlx` queries over [`reelix_db::Db`]).

pub mod authors;
pub mod books;
pub mod editions;
pub mod files;
pub mod series;

use chrono::NaiveDate;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// `?, ?, ?` for an `IN (…)` list of `n` items (`NULL` when empty, matching nothing).
pub(crate) fn placeholders(n: usize) -> String {
    if n == 0 {
        return "NULL".into();
    }
    vec!["?"; n].join(", ")
}

pub(crate) fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub(crate) fn from_json<T: DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub(crate) fn date_str(d: Option<NaiveDate>) -> Option<String> {
    d.map(|d| d.format("%Y-%m-%d").to_string())
}

pub(crate) fn parse_date(s: Option<String>) -> Option<NaiveDate> {
    s.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
}

/// Lower-case alphanumeric key for matching (`"The Way of Kings"` → `"wayofkings"`).
pub(crate) fn clean(title: &str) -> String {
    reelix_import::matching::normalize_title(title).replace(' ', "")
}
