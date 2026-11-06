//! The authenticated app shell (sidebar + topbar + routed content) and global shortcuts.

use std::cell::Cell;
use std::rc::Rc;

use leptos::ev;
use leptos::prelude::*;
use leptos_router::components::{A, Outlet};
use leptos_router::hooks::{use_location, use_navigate};
use reelix_api_types::migrate::PendingMigrationSource;
use reelix_api_types::system::UiSettings;
use wasm_bindgen::JsCast;

use super::sidebar::Sidebar;
use super::topbar::{SEARCH_ID, Topbar};
use super::{Icon, Spinner};
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

    // Load the user's display preferences before showing pages, so dates render in their format.
    let prefs_for = RwSignal::new(None::<i64>);
    Effect::new(move |_| {
        let Some(id) = auth.user.with(|u| u.as_ref().map(|u| u.id)) else {
            return;
        };
        if prefs_for.get_untracked() == Some(id) {
            return;
        }
        leptos::task::spawn_local(async move {
            // Defaults apply when the request fails; the pages still work.
            let prefs = crate::api::get::<UiSettings>("/settings/ui")
                .await
                .unwrap_or_default();
            crate::format::set_prefs(prefs);
            prefs_for.set(Some(id));
        });
    });
    let ready = move || {
        auth.user
            .with(|u| u.as_ref().map(|u| u.id))
            .is_some_and(|id| prefs_for.get() == Some(id))
    };

    // On navigation: close the mobile drawer and move focus to the new page's content
    // (not on first load, which would skip the skip link).
    Effect::new(move |prev: Option<String>| {
        let path = location.pathname.get();
        drawer_open.set(false);
        if prev.is_some_and(|p| p != path)
            && let Some(main) = document()
                .get_element_by_id("main")
                .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = main.focus();
        }
        path
    });

    view! {
        <Show
            when=ready
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
                        <MigrationBanner />
                        <Outlet />
                    </main>
                </div>
            </div>
        </Show>
    }
}

/// For admins: points to System → Migrate while Setup (the Windows installer) has saved apps to
/// migrate from. Checked again on navigation until the list is empty.
#[component]
fn MigrationBanner() -> impl IntoView {
    let auth = AuthState::expect();
    let location = use_location();
    let apps = RwSignal::new(None::<Vec<String>>);
    Effect::new(move |_| {
        let _ = location.pathname.get();
        if !auth.is_admin() || apps.with_untracked(|a| a.as_ref().is_some_and(Vec::is_empty)) {
            return;
        }
        leptos::task::spawn_local(async move {
            if let Ok(list) = crate::api::get::<Vec<PendingMigrationSource>>("/system/migrate/pending").await
            {
                let mut names: Vec<String> = list.iter().map(|p| p.source.label().to_owned()).collect();
                names.dedup();
                apps.set(Some(names));
            }
        });
    });
    move || {
        let names = apps.get().unwrap_or_default();
        let on_page = location.pathname.with(|p| p.contains("/system/migrate"));
        (!names.is_empty() && !on_page).then(|| {
            view! {
                <div class="alert alert-info" role="status">
                    <Icon name="download" />
                    <div class="alert-body">
                        <strong>"Ready to migrate"</strong>
                        <span>
                            {format!(
                                "Setup found {} on this PC. Review what Reelix would import, then start the migration.",
                                names.join(", ")
                            )}
                        </span>
                        <div>
                            <A href="/system/migrate" attr:class="btn btn-primary btn-sm">
                                "Review migration"
                            </A>
                        </div>
                    </div>
                </div>
            }
        })
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
