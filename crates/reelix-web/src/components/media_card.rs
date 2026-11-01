//! Discovery building blocks: poster cards, horizontal rows, availability badges.

use leptos::html;
use leptos::prelude::*;
use leptos_router::components::A;

use super::Icon;

/// Full URL of a TMDB image path (`/abc.jpg`) at the given size (`w342`, `w780`, `original`…).
pub fn tmdb_image(path: Option<&str>, size: &str) -> Option<String> {
    let path = path?.trim();
    if path.is_empty() {
        return None;
    }
    if path.starts_with("http://") || path.starts_with("https://") {
        return Some(path.to_string());
    }
    let sep = if path.starts_with('/') { "" } else { "/" };
    Some(format!("https://image.tmdb.org/t/p/{size}{sep}{path}"))
}

/// Visual state of a corner badge on a poster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeKind {
    Available,
    Partial,
    Requested,
    Processing,
}

impl BadgeKind {
    pub fn class(self) -> &'static str {
        match self {
            Self::Available => "media-badge available",
            Self::Partial => "media-badge partial",
            Self::Requested => "media-badge requested",
            Self::Processing => "media-badge processing",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Available => "✓",
            Self::Partial => "◐",
            Self::Requested => "⏳",
            Self::Processing => "⟳",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Available => "Available",
            Self::Partial => "Partially available",
            Self::Requested => "Requested",
            Self::Processing => "Processing",
        }
    }
}

/// Small pill shown in a poster corner (and on detail pages).
#[component]
pub fn MediaBadge(kind: BadgeKind, #[prop(optional)] large: bool) -> impl IntoView {
    view! {
        <span class=kind.class() class:large=large title=kind.label()>
            <span aria-hidden="true">{kind.glyph()}</span>
            <span class="media-badge-label">{kind.label()}</span>
        </span>
    }
}

/// A poster tile linking to a detail page.
#[component]
pub fn MediaCard(
    #[prop(into)] href: String,
    #[prop(into)] title: String,
    #[prop(into, default = None)] subtitle: Option<String>,
    /// TMDB poster path or absolute URL.
    #[prop(into, default = None)]
    poster: Option<String>,
    /// `Movie` / `Series` tag shown on hover.
    #[prop(into, default = None)]
    kind_label: Option<String>,
    #[prop(into, default = None)] badge: Option<BadgeKind>,
) -> impl IntoView {
    let src = tmdb_image(poster.as_deref(), "w342");
    let initials: String = title
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect();
    let label = match &subtitle {
        Some(s) => format!("{title} ({s})"),
        None => title.clone(),
    };
    view! {
        <A href=href attr:class="media-card" attr:title=label>
            <div class="media-poster">
                {match src {
                    Some(src) => {
                        view! { <img src=src alt="" loading="lazy" decoding="async" /> }.into_any()
                    }
                    None => view! { <span class="media-poster-fallback" aria-hidden="true">{initials}</span> }.into_any(),
                }}
                {kind_label.map(|k| {
                    let icon = if k.eq_ignore_ascii_case("movie") { "film" } else { "tv" };
                    view! { <span class="media-kind"><Icon name=icon />{k}</span> }
                })}
                {badge.map(|b| view! { <MediaBadge kind=b /> })}
            </div>
            <div class="media-meta">
                <span class="media-title">{title}</span>
                {subtitle.map(|s| view! { <span class="media-sub">{s}</span> })}
            </div>
        </A>
    }
}

/// Placeholder tiles while a row loads.
#[component]
pub fn MediaCardSkeleton(#[prop(default = 8)] count: usize) -> impl IntoView {
    (0..count)
        .map(|_| {
            view! {
                <div class="media-card skeleton-card" aria-hidden="true">
                    <div class="media-poster skeleton"></div>
                    <div class="skeleton short"></div>
                </div>
            }
        })
        .collect_view()
}

/// A titled, horizontally scrolling row of cards with arrow buttons.
#[component]
pub fn MediaRow(
    #[prop(into)] title: String,
    #[prop(optional, into)] see_all: Option<String>,
    children: Children,
) -> impl IntoView {
    let track = NodeRef::<html::Div>::new();
    let scroll = move |dir: f64| {
        if let Some(el) = track.get() {
            let by = f64::from(el.client_width()) * 0.85 * dir;
            el.scroll_by_with_x_and_y(by, 0.0);
        }
    };
    view! {
        <section class="media-row">
            <header class="media-row-head">
                <h2 class="media-row-title">{title.clone()}</h2>
                <div class="media-row-actions">
                    {see_all
                        .map(|href| {
                            view! {
                                <A href=href attr:class="card-link">
                                    "See all"
                                </A>
                            }
                        })}
                    <button type="button" class="icon-btn icon-btn-sm" aria-label=format!("Scroll {title} left")
                        on:click=move |_| scroll(-1.0)>
                        <Icon name="chevron" class="flip" />
                    </button>
                    <button type="button" class="icon-btn icon-btn-sm" aria-label=format!("Scroll {title} right")
                        on:click=move |_| scroll(1.0)>
                        <Icon name="chevron" />
                    </button>
                </div>
            </header>
            <div class="media-track" node_ref=track>
                {children()}
            </div>
        </section>
    }
}

/// Responsive wrapping grid of cards (search results, "see all").
#[component]
pub fn MediaGrid(children: Children) -> impl IntoView {
    view! { <div class="media-grid">{children()}</div> }
}
