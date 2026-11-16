//! Shared building blocks for the library screens: view switcher, pager, sort controls,
//! bulk-selection bar, live refresh hook and a few formatting helpers.

use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::*;
use reelix_api_types::DomainEvent;

use crate::ws::LiveEvents;

/// `1.4 GB`, `820 MB`, `0 B`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// `2h 15m` from minutes.
pub fn runtime(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// Reads a per-browser UI preference.
pub fn pref_get(key: &str) -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item(&format!("reelix.{key}"))
        .ok()?
}

/// Stores a per-browser UI preference (best effort).
pub fn pref_set(key: &str, value: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(&format!("reelix.{key}"), value);
    }
}

/// Timer jobs that finish every few seconds or minutes and do not change library rows in bulk. A page
/// that refetches "on jobs" ignores them: it still refetches on the `entity.changed` events they publish.
const QUIET_JOBS: &[&str] = &[
    "download_monitor",
    "rss_sync",
    "process_pending",
    "client_health",
    "indexer_health",
    "health_watch",
    "request_status_sync",
    "definitions_sync",
    "update_check",
    "watchlist_sync",
    "media_server_availability",
    "housekeeping",
    "backup",
    "prune_finished",
    "prune_history",
];
/// Changes that arrive together (a refresh sends one event per item) cause one refetch, this long after
/// the first.
const SETTLE: Duration = Duration::from_millis(1500);
/// While the tab is hidden, a waiting refetch looks again this often, and runs once the tab is shown.
const HIDDEN_RECHECK: Duration = Duration::from_secs(5);

fn tab_hidden() -> bool {
    document().hidden()
}

fn refetch_later(pending: StoredValue<bool>, refetch: Rc<dyn Fn()>, after: Duration) {
    set_timeout(
        move || {
            // The page was left, and its resources with it.
            if pending.try_get_value().is_none() {
                return;
            }
            if tab_hidden() {
                refetch_later(pending, refetch, HIDDEN_RECHECK);
                return;
            }
            pending.set_value(false);
            refetch();
        },
        after,
    );
}

/// Calls `refetch` when an `entity.changed` event arrives for one of `entities` (or a job finishes,
/// when `on_jobs` is set: refresh and scan jobs change many rows at once). Events that arrive together
/// cause one refetch after [`SETTLE`], and a hidden tab waits until it is shown, so an open library page
/// no longer reloads the whole library for every timer job and every item of a refresh.
pub fn on_entity_changed(entities: &'static [&'static str], on_jobs: bool, refetch: impl Fn() + 'static) {
    let live = LiveEvents::expect();
    let refetch: Rc<dyn Fn()> = Rc::new(refetch);
    let pending = StoredValue::new(false);
    Effect::new(move |prev: Option<u64>| {
        let latest = live.latest();
        let seq = latest.as_ref().map_or(0, |e| e.seq);
        if let (Some(prev), Some(ev)) = (prev, latest)
            && ev.seq != prev
        {
            let hit = match &ev.event {
                DomainEvent::EntityChanged { entity, .. } => entities.iter().any(|e| e == entity),
                DomainEvent::JobFinished { kind, .. } => on_jobs && !QUIET_JOBS.contains(&kind.as_str()),
                _ => false,
            };
            if hit && !pending.get_value() {
                pending.set_value(true);
                refetch_later(pending, refetch.clone(), SETTLE);
            }
        }
        seq
    });
}

/// Extra glyphs used by the library screens (24×24, stroke-based like [`crate::components::Icon`]).
#[component]
pub fn LibIcon(#[prop(into)] name: String) -> impl IntoView {
    let markup = match name.as_str() {
        "grid" => {
            r#"<rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="9" rx="1.5"/><rect x="3" y="15" width="7" height="6" rx="1.5"/><rect x="14" y="15" width="7" height="6" rx="1.5"/>"#
        }
        "rows" => {
            r#"<rect x="3" y="4" width="5" height="7" rx="1"/><path d="M11 6h10M11 9h6"/><rect x="3" y="13" width="5" height="7" rx="1"/><path d="M11 15h10M11 18h6"/>"#
        }
        "table" => r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M3 14h18M9 9v11"/>"#,
        "edit" => r#"<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>"#,
        "rename" => r#"<path d="M4 7V5h12v2M10 5v14M7 19h6"/><path d="M17 13l3 3-3 3"/>"#,
        "select" => r#"<rect x="3" y="3" width="18" height="18" rx="3"/><path d="M8 12l3 3 5-6"/>"#,
        "bookmark" => r#"<path d="M6 3h12v18l-6-4-6 4z"/>"#,
        "bookmark-off" => r#"<path d="M6 3h12v18l-6-4-6 4z"/><path d="M3 3l18 18"/>"#,
        "person" => r#"<circle cx="12" cy="8" r="4"/><path d="M4 21c1.5-4 4.5-6 8-6s6.5 2 8 6"/>"#,
        "filter" => r#"<path d="M3 5h18l-7 8v6l-4 2v-8z"/>"#,
        "film" => {
            r#"<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M7 3v18M17 3v18M3 8h4M3 12h4M3 16h4M17 8h4M17 12h4M17 16h4"/>"#
        }
        _ => r#"<circle cx="12" cy="12" r="8"/>"#,
    };
    view! {
        <svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" inner_html=markup></svg>
    }
}

/// How a library list is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Posters,
    Overview,
    Table,
}

impl ViewMode {
    pub const ALL: [Self; 3] = [Self::Posters, Self::Overview, Self::Table];

    pub fn key(self) -> &'static str {
        match self {
            Self::Posters => "posters",
            Self::Overview => "overview",
            Self::Table => "table",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Posters => "Posters",
            Self::Overview => "Overview",
            Self::Table => "Table",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Posters => "grid",
            Self::Overview => "rows",
            Self::Table => "table",
        }
    }

    /// The remembered view for `scope` (e.g. `movies`), defaulting to the UI settings' library view.
    pub fn load(scope: &str) -> Self {
        let saved = pref_get(&format!("{scope}.view"))
            .unwrap_or_else(|| crate::format::prefs().library_view.as_str().to_owned());
        Self::ALL
            .into_iter()
            .find(|m| saved == m.key())
            .unwrap_or(Self::Posters)
    }
}

/// Segmented control to switch between poster / overview / table views (remembered per scope).
#[component]
pub fn ViewSwitcher(mode: RwSignal<ViewMode>, scope: &'static str) -> impl IntoView {
    view! {
        <div class="segmented" role="group" aria-label="View">
            {ViewMode::ALL
                .into_iter()
                .map(|m| {
                    view! {
                        <button
                            type="button"
                            class="seg-btn"
                            class:active=move || mode.get() == m
                            aria-pressed=move || (mode.get() == m).to_string()
                            title=m.label()
                            on:click=move |_| {
                                mode.set(m);
                                pref_set(&format!("{scope}.view"), m.key());
                            }
                        >
                            <LibIcon name=m.icon() />
                            <span class="seg-label">{m.label()}</span>
                        </button>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// Generic segmented control over string options `(value, label)`.
#[component]
pub fn Segmented(
    value: RwSignal<String>,
    options: Vec<(&'static str, &'static str)>,
    #[prop(into)] label: String,
) -> impl IntoView {
    view! {
        <div class="segmented" role="group" aria-label=label>
            {options
                .into_iter()
                .map(|(v, l)| {
                    view! {
                        <button
                            type="button"
                            class="seg-btn"
                            class:active=move || value.get() == v
                            aria-pressed=move || (value.get() == v).to_string()
                            on:click=move |_| value.set(v.to_string())
                        >
                            {l}
                        </button>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// Prev / next pager with a "showing x–y of z" summary.
#[component]
pub fn Pager(page: RwSignal<u32>, page_size: u32, #[prop(into)] total: Signal<u64>) -> impl IntoView {
    let pages = move || total.get().div_ceil(u64::from(page_size.max(1))).max(1) as u32;
    let summary = move || {
        let t = total.get();
        if t == 0 {
            return "No results".to_string();
        }
        let from = u64::from(page.get().saturating_sub(1)) * u64::from(page_size) + 1;
        let to = (from + u64::from(page_size) - 1).min(t);
        format!("{from}–{to} of {t}")
    };
    view! {
        <nav class="pager" aria-label="Pagination">
            <span class="pager-summary">{summary}</span>
            <div class="pager-buttons">
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || { page.get() <= 1 }
                    on:click=move |_| page.update(|p| *p = p.saturating_sub(1).max(1))>
                    "‹ Prev"
                </button>
                <span class="pager-page">{move || format!("Page {} of {}", page.get(), pages())}</span>
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || { page.get() >= pages() }
                    on:click=move |_| page.update(|p| *p += 1)>
                    "Next ›"
                </button>
            </div>
        </nav>
    }
}

/// Sticky bar shown while items are selected: count, select all / none and action buttons.
#[component]
pub fn BulkBar(
    #[prop(into)] count: Signal<usize>,
    on_all: Callback<()>,
    on_none: Callback<()>,
    on_done: Callback<()>,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="bulk-bar" role="region" aria-label="Bulk actions">
            <span class="bulk-count">{move || format!("{} selected", count.get())}</span>
            <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| on_all.run(())>"Select all"</button>
            <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| on_none.run(())>"Clear"</button>
            <div class="bulk-actions">{children()}</div>
            <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| on_done.run(())>"Done"</button>
        </div>
    }
}

/// A clickable table header that toggles sort on `key`.
#[component]
pub fn SortHeader(
    #[prop(into)] label: String,
    key: &'static str,
    sort: RwSignal<String>,
    desc: RwSignal<bool>,
) -> impl IntoView {
    let active = move || sort.get() == key;
    view! {
        <th scope="col" aria-sort=move || if active() { if desc.get() { "descending" } else { "ascending" } } else { "none" }>
            <button type="button" class="th-sort" class:active=active on:click=move |_| {
                if sort.get_untracked() == key {
                    desc.update(|d| *d = !*d);
                } else {
                    sort.set(key.to_string());
                    desc.set(false);
                }
            }>
                {label}
                <span class="th-arrow" aria-hidden="true">
                    {move || if !active() { "" } else if desc.get() { "▼" } else { "▲" }}
                </span>
            </button>
        </th>
    }
}

/// Selection set helper: toggle membership of `id`.
pub fn toggle_id(set: RwSignal<Vec<i64>>, id: i64) {
    set.update(|s| {
        if let Some(pos) = s.iter().position(|x| *x == id) {
            s.remove(pos);
        } else {
            s.push(id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
        assert_eq!(runtime(135), "2h 15m");
        assert_eq!(runtime(45), "45m");
    }
}
