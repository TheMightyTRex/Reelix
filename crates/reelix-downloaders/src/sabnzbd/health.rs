//! SABnzbd health checks (README "Health Checks"), evaluated from `mode=get_config`.

use serde_json::Value;

use crate::common::version_at_least;
use crate::model::{HealthKind, HealthWarning, OutputDir};

/// Minimum supported SABnzbd version.
pub const MIN_VERSION: &str = "3.0.0";

/// Whether `version` is supported. Development builds (`develop`, no digits) are accepted.
pub fn version_supported(version: &str) -> bool {
    !version.trim_start().starts_with(|c: char| c.is_ascii_digit()) || version_at_least(version, MIN_VERSION)
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => matches!(s.trim(), "1" | "true" | "True" | "on"),
        _ => false,
    }
}

/// List value that may be a JSON array or a comma-separated string.
fn list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.trim().to_owned()))
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Joins SABnzbd's `complete_dir` and a category folder (absolute folders win).
fn join_dir(complete: &str, dir: &str) -> String {
    let dir = dir.trim().trim_end_matches('*');
    if dir.is_empty() {
        return complete.trim_end_matches(['/', '\\']).to_owned();
    }
    let absolute = dir.starts_with('/') || dir.starts_with('\\') || dir.as_bytes().get(1) == Some(&b':');
    if absolute || complete.is_empty() {
        return dir.trim_end_matches(['/', '\\']).to_owned();
    }
    let sep = if complete.contains('\\') && !complete.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!(
        "{}{sep}{}",
        complete.trim_end_matches(['/', '\\']),
        dir.trim_end_matches(['/', '\\'])
    )
}

/// Output folder per configured category (plus the default `complete_dir`).
pub fn output_dirs(config: &Value, categories: &[String]) -> Vec<OutputDir> {
    let complete = config
        .get("misc")
        .map(|m| str_of(m, "complete_dir"))
        .unwrap_or("");
    let cats = config
        .get("categories")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    if !complete.is_empty() {
        out.push(OutputDir {
            category: None,
            path: complete.trim_end_matches(['/', '\\']).to_owned(),
        });
    }
    for name in categories {
        if let Some(c) = cats.iter().find(|c| str_of(c, "name").eq_ignore_ascii_case(name)) {
            let path = join_dir(complete, str_of(c, "dir"));
            if !path.is_empty() {
                out.push(OutputDir {
                    category: Some(name.clone()),
                    path,
                });
            }
        }
    }
    out
}

/// Configured categories that do not exist in SABnzbd.
pub fn missing_categories(available: &[String], configured: &[String]) -> Vec<String> {
    configured
        .iter()
        .filter(|c| !available.iter().any(|a| a.eq_ignore_ascii_case(c)))
        .cloned()
        .collect()
}

/// Every check that can be decided from the full config.
pub fn evaluate(config: &Value, version: Option<&str>, categories: &[String]) -> Vec<HealthWarning> {
    let mut w = Vec::new();
    if let Some(v) = version.filter(|v| !version_supported(v)) {
        w.push(
            HealthWarning::new(
                HealthKind::VersionTooOld,
                format!("SABnzbd {v} is not supported; please upgrade to {MIN_VERSION} or newer"),
            )
            .at("/config/"),
        );
    }

    let available: Vec<String> = config
        .get("categories")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|c| str_of(c, "name").to_owned()).collect())
        .unwrap_or_default();
    for c in missing_categories(&available, categories) {
        w.push(
            HealthWarning::new(
                HealthKind::CategoryMissing,
                format!("Category `{c}` does not exist in SABnzbd"),
            )
            .at("/config/categories/"),
        );
    }

    let misc = config.get("misc").cloned().unwrap_or(Value::Null);
    for c in categories {
        for kind in sorting_for(config, &misc, c) {
            w.push(
                HealthWarning::new(
                    HealthKind::SortingEnabled,
                    format!(
                        "{kind} sorting is enabled for category `{c}`; SABnzbd renames files before Reelix can match them"
                    ),
                )
                .at("/config/sorting/"),
            );
        }
    }

    if history_deletes_immediately(&misc) {
        w.push(
            HealthWarning::new(
                HealthKind::HistoryRetention,
                "SABnzbd deletes completed jobs from history immediately, so Reelix cannot see finished downloads",
            )
            .at("/config/switches/"),
        );
    }
    w
}

/// Sorters that apply to `category`: 4.x `sorters` list, else 3.x `misc.*_sorting` switches.
fn sorting_for(config: &Value, misc: &Value, category: &str) -> Vec<String> {
    let matches = |cats: &[String]| cats.iter().any(|c| c == "*" || c.eq_ignore_ascii_case(category));
    if let Some(sorters) = config.get("sorters").and_then(Value::as_array) {
        return sorters
            .iter()
            .filter(|s| truthy(s.get("is_active")) && matches(&list(s.get("sort_cats"))))
            .map(|s| {
                let n = str_of(s, "name");
                if n.is_empty() {
                    "A".to_owned()
                } else {
                    format!("`{n}`")
                }
            })
            .collect();
    }
    [("TV", "tv"), ("Movie", "movie"), ("Date", "date")]
        .into_iter()
        .filter(|(_, k)| {
            truthy(misc.get(format!("enable_{k}_sorting").as_str()))
                && matches(&list(misc.get(format!("{k}_categories").as_str())))
        })
        .map(|(label, _)| label.to_owned())
        .collect()
}

/// 4.x: `history_retention_option = all-delete` (or keep 0 jobs/days);
/// 3.x: `history_retention = "-1"`.
fn history_deletes_immediately(misc: &Value) -> bool {
    match str_of(misc, "history_retention_option") {
        "all-delete" => return true,
        "number-delete" | "days-delete" => {
            let n = misc.get("history_retention_number");
            let zero = match n {
                Some(Value::Number(n)) => n.as_i64() == Some(0),
                Some(Value::String(s)) => s.trim() == "0",
                _ => false,
            };
            return zero;
        }
        "" => {}
        _ => return false,
    }
    str_of(misc, "history_retention").trim() == "-1"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn join_dirs() {
        assert_eq!(join_dir("/data/complete", "movies"), "/data/complete/movies");
        assert_eq!(join_dir("/data/complete/", "/mnt/tv/"), "/mnt/tv");
        assert_eq!(join_dir("D:\\complete", "tv"), "D:\\complete\\tv");
        assert_eq!(join_dir("/c", ""), "/c");
        assert_eq!(join_dir("/c", "movies*"), "/c/movies");
    }

    #[test]
    fn retention() {
        assert!(history_deletes_immediately(
            &json!({"history_retention_option": "all-delete"})
        ));
        assert!(history_deletes_immediately(
            &json!({"history_retention_option": "number-delete", "history_retention_number": 0})
        ));
        assert!(!history_deletes_immediately(
            &json!({"history_retention_option": "all"})
        ));
        assert!(history_deletes_immediately(&json!({"history_retention": "-1"})));
        assert!(!history_deletes_immediately(&json!({"history_retention": ""})));
    }

    #[test]
    fn versions() {
        assert!(version_supported("4.3.2"));
        assert!(version_supported("develop"));
        assert!(!version_supported("2.3.9"));
    }
}
