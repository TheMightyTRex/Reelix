//! The authenticated app shell (sidebar + topbar + routed content) and global shortcuts.

use std::cell::Cell;
use std::rc::Rc;

use leptos::ev;
use leptos::prelude::*;
use leptos_router::components::Outlet;
use leptos_router::hooks::{use_location, use_navigate};
use wasm_bindgen::JsCast;

use super::Spinner;
use super::sidebar::Sidebar;
use super::topbar::{SEARCH_ID, Topbar};
use crate::state::{AuthState, Boot};

/// Layout for every signed-in page. Redirects to `/setup` or `/login` when appropriate.
#[component]
pub fn Shell() -> impl IntoView {
    let auth = AuthState::expect();
    let navigate = use_navigate();
    let location = use_location();
    let drawer_open = RwSignal::new(false);

    Effect::new(move |_| match auth.boot.get() {
        Boot::NeedsSetup => navigate("/setup", Default::default()),
        Boot::Ready if !auth.is_logged_in() => navigate("/login", Default::default()),
        _ => {}
    });

    // Close the mobile drawer whenever the route changes.
    Effect::new(move |_| {
        location.pathname.track();
        drawer_open.set(false);
    });

    view! {
        <Show
            when=move || auth.is_logged_in()
            fallback=|| {
                view! {
                    <div class="splash">
                        <Spinner />
                    </div>
                }
            }
        >
            <a class="skip-link" href="#main">
                "Skip to content"
            </a>
            <div class="shell" class:drawer-open=move || drawer_open.get()>
                <Sidebar open=drawer_open />
                <div class="shell-main">
                    <Topbar drawer_open=drawer_open />
                    <main id="main" class="content" tabindex="-1">
                        <Outlet />
                    </main>
                </div>
            </div>
        </Show>
    }
}

fn typing_in_field(e: &ev::KeyboardEvent) -> bool {
    e.target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .is_some_and(|el| {
            let tag = el.tag_name().to_ascii_lowercase();
            matches!(tag.as_str(), "input" | "textarea" | "select") || el.has_attribute("contenteditable")
        })
}

/// Keyboard shortcuts: `/` focuses search; `g d|m|s|q|t` jump to dashboard, movies, series, queue, tasks.
#[component]
pub fn GlobalShortcuts() -> impl IntoView {
    let navigate = use_navigate();
    let auth = AuthState::expect();
    let pending_g = Rc::new(Cell::new(false));
    // Lives for the whole app, so the handle is intentionally never removed.
    let _handle = window_event_listener(ev::keydown, move |e| {
        if e.ctrl_key() || e.meta_key() || e.alt_key() || typing_in_field(&e) || !auth.is_logged_in() {
            return;
        }
        let key = e.key();
        if pending_g.replace(false) {
            let target = match key.as_str() {
                "d" => Some("/"),
                "m" => Some("/movies"),
                "s" => Some("/series"),
                "q" => Some("/activity/queue"),
                "t" => Some("/system/tasks"),
                _ => None,
            };
            if let Some(path) = target {
                e.prevent_default();
                navigate(path, Default::default());
            }
            return;
        }
        match key.as_str() {
            "/" => {
                if let Some(el) = document().get_element_by_id(SEARCH_ID) {
                    e.prevent_default();
                    let _ = el.unchecked_into::<web_sys::HtmlElement>().focus();
                }
            }
            "g" => pending_g.set(true),
            _ => {}
        }
    });
}
