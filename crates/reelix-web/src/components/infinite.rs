//! Infinite scrolling: [`InfiniteSentinel`] asks for the next batch as the end of a list nears
//! the viewport, [`PagedList`] accumulates server pages behind it, and [`remember_window`]
//! brings a library page back to where the user left it after Back.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{IntersectionObserver, IntersectionObserverEntry, IntersectionObserverInit};

use super::Spinner;
use crate::api::{ApiError, ApiResult};

/// Start loading this far below the viewport, so the next batch is usually in before the user
/// reaches the end.
const ROOT_MARGIN: &str = "0px 0px 800px 0px";

// ---------- pure helpers ----------

/// Appends the items of `batch` whose key is not in `into` yet; returns how many were added.
/// Rows can shift between pages while new ones arrive, so a page may repeat a row already shown.
pub fn merge_unique<T, K: PartialEq>(into: &mut Vec<T>, batch: Vec<T>, key: impl Fn(&T) -> K) -> usize {
    let before = into.len();
    for item in batch {
        let k = key(&item);
        if !into.iter().any(|x| key(x) == k) {
            into.push(item);
        }
    }
    into.len() - before
}

/// Whether a server-paged list has more to fetch.
pub fn has_more(loaded: usize, total: u64, exhausted: bool) -> bool {
    !exhausted && (loaded as u64) < total
}

/// The size of a client-side window (a multiple of `step`) that includes item `index`.
pub fn window_covering(index: usize, step: usize) -> usize {
    let step = step.max(1);
    (index / step + 1) * step
}

/// `shown|scroll_y|signature`, as kept in session storage.
fn format_saved(shown: usize, y: f64, signature: &str) -> String {
    format!("{shown}|{y:.0}|{signature}")
}

/// Parses [`format_saved`]: `(shown, scroll_y, signature)`.
fn parse_saved(s: &str) -> Option<(usize, f64, String)> {
    let mut parts = s.splitn(3, '|');
    let shown = parts.next()?.parse().ok()?;
    let y: f64 = parts.next()?.parse().ok()?;
    let sig = parts.next()?.to_string();
    (y.is_finite() && y >= 0.0).then_some((shown, y, sig))
}

// ---------- sentinel ----------

type ObserverSlot = Option<(IntersectionObserver, Closure<dyn FnMut(js_sys::Array)>)>;

/// Sits after a list and calls `on_visible` when it comes within ~800px of the viewport while
/// `enabled` (there is more) and not `loading`. Also renders a "Load more" button — for keyboard
/// users and for browsers without `IntersectionObserver` — a spinner while loading, an optional
/// "Showing N of M" summary and a polite "Loaded N more" announcement.
#[component]
pub fn InfiniteSentinel(
    /// Asks for the next batch.
    on_visible: Callback<()>,
    /// There is more to load.
    #[prop(into)]
    enabled: Signal<bool>,
    /// A batch is in flight: shows the spinner and pauses the observer.
    #[prop(optional, into)]
    loading: Option<Signal<bool>>,
    /// Items loaded so far (for the summary and the announcement).
    #[prop(optional, into)]
    loaded: Option<Signal<usize>>,
    /// Items in total (for the summary).
    #[prop(optional, into)]
    total: Option<Signal<u64>>,
) -> impl IntoView {
    let loading = loading.unwrap_or_else(|| Signal::stored(false));
    let el = NodeRef::<leptos::html::Div>::new();
    let observer = StoredValue::new_local(None as ObserverSlot);
    let supported = StoredValue::new(true);
    // Bumped after each request, so the observer looks again once the batch has rendered: it
    // only reports changes, and a sentinel still in range would otherwise never fire again.
    let pulse = RwSignal::new(0u32);
    let requested = StoredValue::new(false);
    let announce = RwSignal::new(String::new());

    let request = move || {
        // `try_`: an observer notification can race the page being torn down.
        if enabled.try_get_untracked().unwrap_or(false) && !loading.try_get_untracked().unwrap_or(true) {
            requested.set_value(true);
            on_visible.run(());
            set_timeout(
                move || {
                    let _ = pulse.try_update(|p| *p += 1);
                },
                Duration::from_millis(60),
            );
        }
    };

    Effect::new(move |_| {
        pulse.track();
        let active = enabled.get() && !loading.get();
        let Some(node) = el.get() else { return };
        let target: &web_sys::Element = &node;
        observer.update_value(|slot| {
            if slot.is_none() && supported.get_value() {
                let cb = Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
                    let hit = entries
                        .iter()
                        .any(|e| e.unchecked_into::<IntersectionObserverEntry>().is_intersecting());
                    if hit {
                        request();
                    }
                });
                let init = IntersectionObserverInit::new();
                init.set_root_margin(ROOT_MARGIN);
                match IntersectionObserver::new_with_options(cb.as_ref().unchecked_ref(), &init) {
                    Ok(obs) => *slot = Some((obs, cb)),
                    // No IntersectionObserver: the "Load more" button does the job.
                    Err(_) => supported.set_value(false),
                }
            }
            if let Some((obs, _)) = slot {
                // Re-observing makes the observer report the current state right away.
                obs.unobserve(target);
                if active {
                    obs.observe(target);
                }
            }
        });
    });
    on_cleanup(move || {
        observer.update_value(|slot| {
            if let Some((obs, cb)) = slot.take() {
                obs.disconnect();
                drop(cb);
            }
        });
    });

    if let Some(loaded) = loaded {
        Effect::new(move |prev: Option<usize>| {
            let n = loaded.get();
            match prev {
                Some(p) if n > p && requested.get_value() => {
                    requested.set_value(false);
                    announce.set(format!("Loaded {} more", n - p));
                }
                Some(p) if n < p => requested.set_value(false),
                _ => {}
            }
            n
        });
    }

    let summary = loaded.zip(total).map(|(loaded, total)| {
        move || {
            let t = total.get();
            (t > 0).then(|| {
                let n = (loaded.get() as u64).min(t);
                view! { <p class="infinite-summary">{format!("Showing {n} of {t}")}</p> }
            })
        }
    });

    view! {
        <div class="infinite-sentinel" node_ref=el>
            {summary}
            <Show when=move || loading.get()>
                <Spinner label="Loading more…" />
            </Show>
            <Show when=move || enabled.get() && !loading.get()>
                <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| request()>
                    "Load more"
                </button>
            </Show>
            <p class="visually-hidden" aria-live="polite">{move || announce.get()}</p>
        </div>
    }
}

// ---------- server-paged accumulator ----------

type PageFuture<T> = Pin<Box<dyn Future<Output = ApiResult<(Vec<T>, u64)>>>>;
type PageFetch<T> = Rc<dyn Fn(u32, u32) -> PageFuture<T>>;

/// A server-paged list that grows a page at a time. `fetch(page, page_size)` returns the page's
/// items and the total; it should read its filters untracked, with the page calling
/// [`PagedList::reset`] from an effect that tracks them.
pub struct PagedList<T: Send + Sync + 'static> {
    pub items: RwSignal<Vec<T>>,
    pub total: RwSignal<u64>,
    pub loading: RwSignal<bool>,
    pub error: RwSignal<Option<ApiError>>,
    /// The first page has been answered (until then the list shows a skeleton).
    pub ready: RwSignal<bool>,
    exhausted: RwSignal<bool>,
    pages: StoredValue<u32>,
    /// Bumped by every reset / refresh so answers to earlier requests are ignored.
    generation: StoredValue<u64>,
    page_size: u32,
    key: fn(&T) -> i64,
    fetch: StoredValue<PageFetch<T>, LocalStorage>,
}

impl<T: Send + Sync + 'static> Clone for PagedList<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Send + Sync + 'static> Copy for PagedList<T> {}

impl<T: Clone + Send + Sync + 'static> PagedList<T> {
    pub fn new<F, Fut>(page_size: u32, key: fn(&T) -> i64, fetch: F) -> Self
    where
        F: Fn(u32, u32) -> Fut + 'static,
        Fut: Future<Output = ApiResult<(Vec<T>, u64)>> + 'static,
    {
        let fetch: PageFetch<T> = Rc::new(move |page, size| Box::pin(fetch(page, size)));
        Self {
            items: RwSignal::new(Vec::new()),
            total: RwSignal::new(0),
            loading: RwSignal::new(false),
            error: RwSignal::new(None),
            ready: RwSignal::new(false),
            exhausted: RwSignal::new(false),
            pages: StoredValue::new(0),
            generation: StoredValue::new(0),
            page_size,
            key,
            fetch: StoredValue::new_local(fetch),
        }
    }

    /// More pages to fetch and no error waiting for a retry (tracked).
    pub fn can_load_more(self) -> bool {
        self.error.with(Option::is_none)
            && has_more(self.items.with(Vec::len), self.total.get(), self.exhausted.get())
    }

    /// Items loaded so far (tracked).
    pub fn loaded(self) -> usize {
        self.items.with(Vec::len)
    }

    fn bump(self) -> u64 {
        self.generation.update_value(|g| *g += 1);
        self.generation.get_value()
    }

    /// Starts over from the first page (filters changed). What is shown stays until it answers.
    pub fn reset(self) {
        let generation = self.bump();
        self.pages.set_value(0);
        self.exhausted.set(false);
        self.fetch_page(generation, 1);
    }

    /// Fetches the next page and appends it.
    pub fn load_more(self) {
        if self.loading.get_untracked() {
            return;
        }
        if self.pages.get_value() == 0 {
            return self.reset();
        }
        if !has_more(
            self.items.with_untracked(Vec::len),
            self.total.get_untracked(),
            self.exhausted.get_untracked(),
        ) {
            return;
        }
        self.fetch_page(self.generation.get_value(), self.pages.get_value() + 1);
    }

    /// Retries after an error: the failed page, or the first one.
    pub fn retry(self) {
        self.error.set(None);
        self.load_more();
    }

    fn fetch_page(self, generation: u64, page: u32) {
        self.loading.set(true);
        self.error.set(None);
        let fut = self.fetch.with_value(|f| f(page, self.page_size));
        spawn_local(async move {
            let res = fut.await;
            if self.generation.try_get_value() != Some(generation) {
                return;
            }
            match res {
                Ok((batch, total)) => {
                    let key = self.key;
                    let mut added = 0;
                    self.items.update(|v| {
                        if page == 1 {
                            v.clear();
                        }
                        added = merge_unique(v, batch, key);
                    });
                    self.pages.set_value(page);
                    self.total.set(total);
                    // A page with nothing new means the server has run dry; stop asking.
                    if page > 1 && added == 0 {
                        self.exhausted.set(true);
                    }
                }
                Err(e) => self.error.set(Some(e)),
            }
            self.ready.set(true);
            self.loading.set(false);
        });
    }

    /// Re-fetches every loaded page (live updates) and swaps the rows in place, keeping the
    /// loaded range — and so the scroll position. On failure the current rows stay.
    pub fn refresh(self) {
        let pages = self.pages.get_value();
        if pages == 0 {
            return self.reset();
        }
        let generation = self.bump();
        self.loading.set(true);
        let futs: Vec<_> = self
            .fetch
            .with_value(|f| (1..=pages).map(|p| f(p, self.page_size)).collect());
        spawn_local(async move {
            let results = futures::future::join_all(futs).await;
            if self.generation.try_get_value() != Some(generation) {
                return;
            }
            let mut merged = Vec::new();
            let mut total = None;
            for res in results {
                match res {
                    Ok((batch, t)) => {
                        merge_unique(&mut merged, batch, self.key);
                        total = Some(t);
                    }
                    Err(_) => {
                        total = None;
                        break;
                    }
                }
            }
            if let Some(t) = total {
                self.items.set(merged);
                self.total.set(t);
                self.exhausted.set(false);
                self.error.set(None);
            }
            self.loading.set(false);
        });
    }
}

// ---------- scroll restoration ----------

thread_local! {
    static LAST_POPSTATE: Cell<f64> = const { Cell::new(f64::NEG_INFINITY) };
}

/// Notes the time of every Back / Forward. Call once before the router mounts, so this
/// listener runs before the router's own.
pub fn track_history_traversal() {
    // App-lifetime listener: never removed.
    let _ = window_event_listener(leptos::ev::popstate, |_| {
        LAST_POPSTATE.with(|t| t.set(js_sys::Date::now()));
    });
}

/// The page is being shown because of Back / Forward (a popstate just happened).
fn came_back() -> bool {
    LAST_POPSTATE.with(|t| js_sys::Date::now() - t.get() < 1500.0)
}

fn session_get(key: &str) -> Option<String> {
    web_sys::window()?.session_storage().ok()??.get_item(key).ok()?
}

fn session_set(key: &str, value: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
        let _ = storage.set_item(key, value);
    }
}

/// Remembers how much of a client-side window (`shown`) was rendered and how far the page was
/// scrolled when leaving it; coming back with Back / Forward with the same `signature` (the
/// filters) restores both once `ready` (the list has loaded).
pub fn remember_window(
    key: &'static str,
    shown: RwSignal<usize>,
    signature: impl Fn() -> String + Send + Sync + 'static,
    ready: impl Fn() -> bool + Send + Sync + 'static,
) {
    let storage_key = format!("reelix.scroll.{key}");
    let target_y = StoredValue::new(None::<f64>);
    if came_back()
        && let Some((n, y, sig)) = session_get(&storage_key).as_deref().and_then(parse_saved)
        && sig == untrack(&signature)
    {
        shown.set(n.max(shown.get_untracked()));
        target_y.set_value(Some(y));
    }

    // Scroll events arrive a frame late, so the router's scroll-to-top on the way out has not
    // been recorded yet when the page is cleaned up.
    let last_y = StoredValue::new(0.0f64);
    let handle = window_event_listener(leptos::ev::scroll, move |_| {
        let _ = last_y.try_set_value(window().scroll_y().unwrap_or(0.0));
    });
    on_cleanup(move || {
        handle.remove();
        if let (Some(n), Some(y)) = (shown.try_get_untracked(), last_y.try_get_value()) {
            session_set(&storage_key, &format_saved(n, y, &untrack(&signature)));
        }
    });

    Effect::new(move |_| {
        if target_y.get_value().is_none() || !ready() {
            return;
        }
        let y = target_y.get_value().unwrap_or(0.0);
        target_y.set_value(None);
        set_timeout(
            move || window().scroll_to_with_x_and_y(0.0, y),
            Duration::from_millis(30),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_without_duplicates() {
        let mut v = vec![1, 2, 3];
        assert_eq!(merge_unique(&mut v, vec![3, 4, 4, 5], |x| *x), 2);
        assert_eq!(v, [1, 2, 3, 4, 5]);
        assert_eq!(merge_unique(&mut v, vec![1, 2], |x| *x), 0);
    }

    #[test]
    fn knows_when_more_pages_exist() {
        assert!(has_more(25, 60, false));
        assert!(!has_more(60, 60, false));
        assert!(!has_more(25, 60, true));
        assert!(!has_more(0, 0, false));
    }

    #[test]
    fn covers_an_index_with_whole_steps() {
        assert_eq!(window_covering(0, 48), 48);
        assert_eq!(window_covering(47, 48), 48);
        assert_eq!(window_covering(48, 48), 96);
        assert_eq!(window_covering(5, 0), 6);
    }

    #[test]
    fn round_trips_saved_windows() {
        let s = format_saved(240, 1834.6, "q=|sort=title|x");
        assert_eq!(
            parse_saved(&s),
            Some((240, 1835.0, "q=|sort=title|x".to_string()))
        );
        assert_eq!(parse_saved("junk"), None);
        assert_eq!(parse_saved("10|-5|"), None);
    }
}
