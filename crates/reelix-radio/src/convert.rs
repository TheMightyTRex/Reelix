//! Conversions between DTO enums, database text columns and timestamps.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Serde wire name of a unit enum variant (`"from_now_on"`).
pub fn enum_str<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// Parses a serde wire name back into the enum.
pub fn parse_enum<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

pub fn json<T: Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub fn from_json<T: DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn ms(t: Option<DateTime<Utc>>) -> Option<i64> {
    t.map(|t| t.timestamp_millis())
}

pub fn from_ms_opt(v: Option<i64>) -> Option<DateTime<Utc>> {
    v.map(reelix_db::from_ms)
}

pub fn u32_opt(v: Option<i64>) -> Option<u32> {
    v.and_then(|v| u32::try_from(v).ok())
}

/// Comparison key for show titles: no case, accents, punctuation or articles
/// (see [`reelix_import::matching::normalize_title`]).
pub fn clean_title(s: &str) -> String {
    reelix_import::matching::normalize_title(s)
}

/// Sort title: a leading "The", "A" or "An" moves out of the way ("The Archers" → "Archers").
pub fn sort_title(title: &str) -> String {
    let t = title.trim();
    for article in ["the ", "a ", "an "] {
        if t.len() > article.len() && t[..article.len()].eq_ignore_ascii_case(article) {
            return t[article.len()..].trim_start().to_owned();
        }
    }
    t.to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sort_titles() {
        assert_eq!(super::sort_title("The Archers"), "Archers");
        assert_eq!(super::sort_title("An Hour"), "Hour");
        assert_eq!(super::sort_title("Theme Time"), "Theme Time");
        assert_eq!(super::sort_title("The"), "The");
        assert_eq!(
            super::clean_title("The News Quiz!"),
            super::clean_title("news quiz")
        );
    }
}
