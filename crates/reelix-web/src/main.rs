//! Reelix web UI: a Leptos client-side-rendered single page app.

mod api;
mod app;
mod components;
mod format;
mod pages;
mod state;
mod ws;

fn main() {
    console_error_panic_hook::set_once();
    components::infinite::track_history_traversal();
    leptos::mount::mount_to_body(app::App);
}
