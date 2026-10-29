//! Library repositories (runtime `sqlx` queries over [`reelix_db::Db`]).

pub mod files;
pub mod misc;
pub mod movies;
pub mod series;

use reelix_core::MediaKind;

/// `?, ?, ?` for an `IN (…)` list of `n` items (`NULL` when empty, matching nothing).
pub(crate) fn placeholders(n: usize) -> String {
    if n == 0 {
        return "NULL".into();
    }
    vec!["?"; n].join(", ")
}

pub(crate) fn kind_str(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
    }
}

pub(crate) fn parse_kind(s: &str) -> MediaKind {
    if s == "series" {
        MediaKind::Series
    } else {
        MediaKind::Movie
    }
}
