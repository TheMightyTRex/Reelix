//! Radio repositories (runtime `sqlx` queries over [`reelix_db::Db`]).

pub mod batch;
pub mod episodes;
pub mod files;
pub mod series;
pub mod shows;
pub mod stations;

/// `?, ?, ?` for an `IN (…)` list of `n` items (`NULL` when empty, matching nothing).
pub(crate) fn placeholders(n: usize) -> String {
    if n == 0 {
        return "NULL".into();
    }
    vec!["?"; n].join(", ")
}

/// `(page, size, Some((offset, limit)))` for a 1-based page; `page_size` 0/None = everything.
pub(crate) fn page_window(page: Option<u32>, page_size: Option<u32>) -> (u32, u32, Option<(i64, i64)>) {
    let page = page.unwrap_or(1).max(1);
    match page_size.filter(|s| *s > 0) {
        Some(size) => (
            page,
            size,
            Some((i64::from(page - 1) * i64::from(size), i64::from(size))),
        ),
        None => (1, 0, None),
    }
}
