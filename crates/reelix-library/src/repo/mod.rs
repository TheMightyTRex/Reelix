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

/// A unique negative TMDB id for a custom movie or series while it is inserted; the insert
/// then sets it to `-id`. Far below any `-id`, so the two never meet.
pub(crate) fn custom_placeholder() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT: AtomicI64 = AtomicI64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed) % 1_000_000;
    -(1_i64 << 50) - reelix_db::now_ms() * 1_000_000 - n
}
