//! Top bar: drawer toggle, global search, live status, theme toggle and user menu.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;

use super::{Icon, Logo};
use crate::api;
use crate::state::auth::role_name;
use crate::state::theme::Mode;
use crate::state::{AuthState, Theme, Toasts};
use crate::ws::{LiveEvents, WsStatus};

/// DOM id of the global search box (focused by the `/` shortcut).
pub const SEARCH_ID: &str = "global-search";

#[component]
pub fn Topbar(drawer_open: RwSignal<bool>) -> impl IntoView {
    let auth = AuthState::expect();
    let theme = Theme::expect();
    let live = LiveEvents::expect();
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let menu_open = RwSignal::new(false);
    let query = RwSignal::new(String::new());
    let submit_search = {
        let navigate = navigate.clone();
        move |e: ev::SubmitEvent| {
            e.prevent_default();
            let q = query.get_untracked().trim().to_string();
            if q.is_empty() {
                return;
            }
            navigate(&crate::pages::search::search_href(&q), Default::default());
        }
    };

    let display_name = move || {
        auth.user
            .with(|u| u.as_ref().map(|u| u.display_name.clone()).unwrap_or_default())
    };
    let username = move || {
        auth.user
            .with(|u| u.as_ref().map(|u| u.username.clone()).unwrap_or_default())
    };
    let role = move || {
        auth.user
            .with(|u| u.as_ref().map(|u| role_name(u.permissions)).unwrap_or_default())
    };
    let initial = move || {
        display_name()
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into())
    };

    let logout = move |_| {
        menu_open.set(false);
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::post_empty::<()>("/auth/logout").await {
                Ok(()) => toasts.info("You have been signed out."),
                Err(e) if e.status == 401 => {}
                Err(e) => toasts.error(format!("Sign-out failed: {e}")),
            }
            auth.clear();
            navigate("/login", Default::default());
        });
    };

    let status_class = move || match live.status.get() {
        WsStatus::Connected => "ws-dot ok",
        WsStatus::Connecting | WsStatus::Reconnecting => "ws-dot pending",
        WsStatus::Idle => "ws-dot off",
    };
    let status_label = move || format!("Live updates: {}", live.status.get().label());

    // A disclosure (button + list of links), not an ARIA menu: Escape closes it and puts focus
    // back on the button.
    let on_menu_keydown = move |e: ev::KeyboardEvent| {
        if e.key() == "Escape" && menu_open.get_untracked() {
            menu_open.set(false);
            if let Some(b) = document()
                .query_selector(".user-button")
                .ok()
                .flatten()
                .and_then(|e| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(e).ok())
            {
                let _ = b.focus();
            }
        }
    };

    view! {
        <header class="topbar">
            <button
                type="button"
                class="icon-btn drawer-toggle"
                aria-label="Open navigation"
                aria-controls="app-sidebar"
                aria-expanded=move || drawer_open.get().to_string()
                on:click=move |_| drawer_open.update(|o| *o = !*o)
            >
                <Icon name="menu" />
            </button>
            <A href="/" attr:class="brand brand-compact" attr:aria-label="Reelix home">
                <Logo />
                <span class="brand-name">"Reelix"</span>
            </A>

            <form class="search" role="search" on:submit=submit_search>
                <Icon name="search" class="search-icon" />
                <label class="visually-hidden" for=SEARCH_ID>
                    "Search"
                </label>
                <input
                    id=SEARCH_ID
                    type="search"
                    class="search-input"
                    placeholder="Search movies, series, people…"
                    autocomplete="off"
                    enterkeyhint="search"
                    prop:value=move || query.get()
                    on:input=move |e| query.set(event_target_value(&e))
                    on:keydown=move |e: ev::KeyboardEvent| {
                        if e.key() == "Escape"
                            && let Some(el) = document().active_element()
                        {
                            let _ = wasm_bindgen::JsCast::unchecked_into::<web_sys::HtmlElement>(el).blur();
                        }
                    }
                />
                <kbd class="search-kbd" aria-hidden="true">
                    "/"
                </kbd>
            </form>

            <div class="topbar-actions">
                <span class="ws-status" title=status_label aria-label=status_label role="status">
                    <span class=status_class aria-hidden="true"></span>
                    <span class="ws-label">{move || live.status.get().label()}</span>
                </span>
                <button
                    type="button"
                    class="icon-btn"
                    aria-label=move || {
                        if theme.mode.get() == Mode::Dark { "Switch to light theme" } else { "Switch to dark theme" }
                    }
                    on:click=move |_| theme.toggle()
                >
                    <Show when=move || theme.mode.get() == Mode::Dark fallback=|| view! { <Icon name="moon" /> }>
                        <Icon name="sun" />
                    </Show>
                </button>

                <div class="user-menu" on:keydown=on_menu_keydown>
                    <button
                        type="button"
                        class="user-button"
                        aria-label=move || format!("Account menu, {}", display_name())
                        aria-expanded=move || menu_open.get().to_string()
                        aria-controls="user-menu"
                        on:click=move |_| menu_open.update(|o| *o = !*o)
                    >
                        <span class="avatar" aria-hidden="true">
                            {initial}
                        </span>
                        <span class="user-name">{display_name}</span>
                    </button>
                    <Show when=move || menu_open.get()>
                        <div class="menu-scrim" on:click=move |_| menu_open.set(false)></div>
                        <div class="menu" id="user-menu">
                            <div class="menu-header">
                                <strong>{display_name}</strong>
                                <span>"@" {username} " · " {role}</span>
                            </div>
                            <A
                                href="/profile"
                                attr:class="menu-item"
                                on:click=move |_| menu_open.set(false)
                            >
                                <Icon name="user" />
                                "Profile & API keys"
                            </A>
                            <button type="button" class="menu-item" on:click=logout.clone()>
                                <Icon name="logout" />
                                "Sign out"
                            </button>
                        </div>
                    </Show>
                </div>
            </div>
        </header>
    }
}
