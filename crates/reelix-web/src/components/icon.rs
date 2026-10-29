//! Inline SVG icon set (stroke icons on a 24×24 grid, drawn for Reelix).

use std::sync::atomic::{AtomicUsize, Ordering};

use leptos::prelude::*;

fn markup(name: &str) -> &'static str {
    match name {
        "dashboard" => {
            r#"<rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/>"#
        }
        "discover" => r#"<circle cx="12" cy="12" r="9"/><path d="M15.5 8.5l-2 5-5 2 2-5z"/>"#,
        "requests" => {
            r#"<path d="M4 6.5h16v3.5a2 2 0 0 0 0 4v3.5H4V14a2 2 0 0 0 0-4z"/><path d="M14 6.5v11" stroke-dasharray="2 2"/>"#
        }
        "issues" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7.5v5.5M12 16.5v.01"/>"#,
        "movies" => {
            r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7.5 4v16M16.5 4v16M3 9h4.5M3 15h4.5M16.5 9H21M16.5 15H21"/>"#
        }
        "series" => {
            r#"<rect x="3" y="6" width="18" height="13" rx="2"/><path d="M8.5 2.5 12 6l3.5-3.5M8 22h8"/>"#
        }
        "calendar" => r#"<rect x="3" y="5" width="18" height="16" rx="2"/><path d="M3 10h18M8 3v4M16 3v4"/>"#,
        "activity" => r#"<path d="M3 12h4l3-7 4 14 3-7h4"/>"#,
        "wanted" => {
            r#"<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="5"/><circle cx="12" cy="12" r="1"/>"#
        }
        "indexers" => {
            r#"<ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/>"#
        }
        "settings" => {
            r#"<path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1"/><circle cx="15" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="17" cy="18" r="2"/>"#
        }
        "system" => {
            r#"<rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><path d="M7 7.5h.01M7 16.5h.01M11 7.5h6M11 16.5h6"/>"#
        }
        "search" => r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>"#,
        "sun" => {
            r#"<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M4.6 4.6 6 6M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4 6 18M18 6l1.4-1.4"/>"#
        }
        "moon" => r#"<path d="M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z"/>"#,
        "menu" => r#"<path d="M4 6h16M4 12h16M4 18h16"/>"#,
        "close" => r#"<path d="M6 6l12 12M18 6 6 18"/>"#,
        "check" => r#"<path d="m5 12.5 4.5 4.5L19 7"/>"#,
        "info" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 11v5.5M12 7.5v.01"/>"#,
        "warning" => r#"<path d="M12 3.5 21.5 20h-19z"/><path d="M12 10v4.5M12 17.5v.01"/>"#,
        "error" => r#"<circle cx="12" cy="12" r="9"/><path d="m9 9 6 6M15 9l-6 6"/>"#,
        "copy" => {
            r#"<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3"/>"#
        }
        "key" => r#"<circle cx="8" cy="15" r="4"/><path d="m11 12 9-9M16 7l3 3M18.5 4.5l2 2"/>"#,
        "trash" => r#"<path d="M4 7h16M9.5 7V4h5v3M6 7l1 13h10l1-13M10 11v5M14 11v5"/>"#,
        "play" => r#"<path d="M8 5v14l11-7z"/>"#,
        "refresh" => r#"<path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5"/>"#,
        "logout" => r#"<path d="M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 17l-5-5 5-5M5 12h11"/>"#,
        "user" => r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#,
        "chevron" => r#"<path d="m9 6 6 6-6 6"/>"#,
        "external" => {
            r#"<path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>"#
        }
        "plus" => r#"<path d="M12 5v14M5 12h14"/>"#,
        "sparkle" => {
            r#"<path d="m12 3 1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z"/><path d="M19 16v4M17 18h4"/>"#
        }
        "download" => r#"<path d="M12 4v11M7 10l5 5 5-5M5 20h14"/>"#,
        "clock" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3.5 2"/>"#,
        "heart" => r#"<path d="M3 12h4l2-4 3 8 2-4h7"/><circle cx="12" cy="12" r="9.5" opacity=".35"/>"#,
        "list" => r#"<path d="M9 6h11M9 12h11M9 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01"/>"#,
        "folder" => {
            r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
        }
        "server" => r#"<rect x="4" y="3" width="16" height="18" rx="2"/><path d="M8 7h8M8 11h8M12 17h.01"/>"#,
        "shield" => {
            r#"<path d="M12 3 4.5 6v5.5c0 4.5 3.2 8.2 7.5 9.5 4.3-1.3 7.5-5 7.5-9.5V6z"/><path d="m9 12 2 2 4-4"/>"#
        }
        "lock" => r#"<rect x="5" y="10" width="14" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/>"#,
        "star" => r#"<path d="m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z"/>"#,
        "film" => {
            r#"<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M7 3v18M17 3v18M3 7.5h4M3 12h4M3 16.5h4M17 7.5h4M17 12h4M17 16.5h4"/>"#
        }
        "tv" => r#"<rect x="3" y="7" width="18" height="12" rx="2"/><path d="m8 3 4 4 4-4M7 22h10"/>"#,
        "books" => {
            r#"<path d="M2 5.5C4.5 4 8 4 12 6.5 16 4 19.5 4 22 5.5V19c-2.5-1.5-6-1.5-10 1-4-2.5-7.5-2.5-10-1z"/><path d="M12 6.5V20"/>"#
        }
        "music" => {
            r#"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"#
        }
        _ => r#"<circle cx="12" cy="12" r="8"/>"#,
    }
}

/// An icon from the built-in set. Decorative by default (`aria-hidden`).
#[component]
pub fn Icon(#[prop(into)] name: String, #[prop(optional, into)] class: Option<String>) -> impl IntoView {
    let class = match class {
        Some(c) => format!("icon {c}"),
        None => "icon".to_string(),
    };
    view! {
        <svg
            class=class
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            focusable="false"
            inner_html=markup(&name)
        ></svg>
    }
}

/// The Reelix brand mark.
#[component]
pub fn Logo() -> impl IntoView {
    // Each instance needs its own gradient id: a gradient inside a hidden element breaks every user.
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let id = format!("reelix-logo-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let fill = format!("url(#{id})");
    // `view!` doesn't know `<stop>` and wraps it in `<g>`, which invalidates the gradient,
    // so the gradient is parsed by the browser from markup instead.
    let defs = format!(
        r#"<linearGradient id="{id}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" style="stop-color: var(--brand-a)"/><stop offset="1" style="stop-color: var(--brand-b)"/></linearGradient>"#
    );
    view! {
        <svg class="logo-mark" viewBox="0 0 64 64" aria-hidden="true" focusable="false">
            <defs inner_html=defs></defs>
            <circle cx="32" cy="32" r="21" fill="none" stroke=fill.clone() stroke-width="6" />
            <path d="M27 21.5v21l16-10.5z" fill=fill />
        </svg>
    }
}
