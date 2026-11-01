//! Small building blocks: badges, empty states, spinners, skeletons, error boxes, cards.

use leptos::prelude::*;
use reelix_api_types::{HealthLevel, JobStatus};

use super::Icon;
use crate::api::ApiError;

/// Colour family of a [`StatusBadge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Success,
    Warning,
    Danger,
    Info,
    Neutral,
    Accent,
}

impl Tone {
    pub fn class(self) -> &'static str {
        match self {
            Self::Success => "tone-success",
            Self::Warning => "tone-warning",
            Self::Danger => "tone-danger",
            Self::Info => "tone-info",
            Self::Neutral => "tone-neutral",
            Self::Accent => "tone-accent",
        }
    }

    pub fn health(level: HealthLevel) -> Self {
        match level {
            HealthLevel::Ok => Self::Success,
            HealthLevel::Warning => Self::Warning,
            HealthLevel::Error => Self::Danger,
        }
    }

    pub fn job(status: JobStatus) -> Self {
        match status {
            JobStatus::Queued => Self::Neutral,
            JobStatus::Running => Self::Info,
            JobStatus::Completed => Self::Success,
            JobStatus::Failed => Self::Danger,
        }
    }
}

/// Label for a health level.
pub fn health_label(level: HealthLevel) -> &'static str {
    match level {
        HealthLevel::Ok => "Healthy",
        HealthLevel::Warning => "Warning",
        HealthLevel::Error => "Error",
    }
}

/// A compact pill showing a status.
#[component]
pub fn StatusBadge(tone: Tone, #[prop(into)] label: String, #[prop(optional)] dot: bool) -> impl IntoView {
    view! {
        <span class=format!("badge {}", tone.class())>
            {dot.then(|| view! { <span class="badge-dot" aria-hidden="true"></span> })}
            {label}
        </span>
    }
}

/// Friendly placeholder for lists and panels with nothing to show.
#[component]
pub fn EmptyState(
    #[prop(into)] icon: String,
    #[prop(into)] title: String,
    #[prop(optional, into)] description: Option<String>,
    #[prop(optional)] compact: bool,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="empty-state" class:compact=compact>
            <div class="empty-icon">
                <Icon name=icon />
            </div>
            <p class="empty-title">{title}</p>
            {description.map(|d| view! { <p class="empty-desc">{d}</p> })}
            {children.map(|c| view! { <div class="empty-actions">{c()}</div> })}
        </div>
    }
}

#[component]
pub fn Spinner(#[prop(optional, into)] label: Option<String>) -> impl IntoView {
    let label = label.unwrap_or_else(|| "Loading…".into());
    view! {
        <span class="spinner" role="status">
            <span class="visually-hidden">{label}</span>
        </span>
    }
}

/// Shimmering placeholder lines shown while data loads.
#[component]
pub fn Skeleton(#[prop(default = 3)] lines: usize) -> impl IntoView {
    view! {
        <div class="skeleton-group" role="status" aria-busy="true">
            <span class="visually-hidden">"Loading…"</span>
            {(0..lines).map(|i| view! { <div class="skeleton" class:short=i % 3 == 2></div> }).collect_view()}
        </div>
    }
}

/// Inline error panel with an optional retry action.
#[component]
pub fn ErrorBox(
    #[prop(into)] error: ApiError,
    #[prop(optional)] on_retry: Option<Callback<()>>,
) -> impl IntoView {
    view! {
        <div class="alert alert-danger" role="alert">
            <Icon name="error" />
            <div class="alert-body">
                <strong>"Something went wrong"</strong>
                <span>{error.message}</span>
            </div>
            {on_retry
                .map(|cb| {
                    view! {
                        <button class="btn btn-sm btn-ghost" on:click=move |_| cb.run(())>
                            <Icon name="refresh" />
                            "Retry"
                        </button>
                    }
                })}
        </div>
    }
}

/// A titled surface. `actions` render on the right of the header.
#[component]
pub fn Card(
    #[prop(optional, into)] title: Option<String>,
    #[prop(optional, into)] subtitle: Option<String>,
    #[prop(optional, into)] class: Option<String>,
    #[prop(optional, into)] actions: Option<AnyView>,
    children: Children,
) -> impl IntoView {
    let has_header = title.is_some() || actions.is_some();
    view! {
        <section class=format!("card {}", class.unwrap_or_default())>
            {has_header
                .then(|| {
                    view! {
                        <header class="card-header">
                            <div>
                                {title.map(|t| view! { <h2 class="card-title">{t}</h2> })}
                                {subtitle.map(|s| view! { <p class="card-subtitle">{s}</p> })}
                            </div>
                            {actions.map(|a| view! { <div class="card-actions">{a}</div> })}
                        </header>
                    }
                })}
            <div class="card-body">{children()}</div>
        </section>
    }
}

/// Title row at the top of every page.
#[component]
pub fn PageHeader(
    #[prop(into)] title: String,
    #[prop(optional, into)] subtitle: Option<String>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    // Each page names the browser tab, so screen-reader users hear where they landed.
    document().set_title(&format!("{title} · Reelix"));
    view! {
        <div class="page-header">
            <div>
                <h1 class="page-title">{title}</h1>
                {subtitle.map(|s| view! { <p class="page-subtitle">{s}</p> })}
            </div>
            {children.map(|c| view! { <div class="page-actions">{c()}</div> })}
        </div>
    }
}

/// A big-number tile for the dashboard.
#[component]
pub fn StatCard(
    #[prop(into)] icon: String,
    #[prop(into)] label: String,
    #[prop(into)] value: Signal<String>,
    #[prop(optional, into)] hint: Option<String>,
    #[prop(optional)] tone: Option<Tone>,
) -> impl IntoView {
    let tone = tone.unwrap_or(Tone::Accent);
    view! {
        <div class=format!("stat-card {}", tone.class())>
            <div class="stat-icon">
                <Icon name=icon />
            </div>
            <div class="stat-text">
                <span class="stat-value">{move || value.get()}</span>
                <span class="stat-label">{label}</span>
                {hint.map(|h| view! { <span class="stat-hint">{h}</span> })}
            </div>
        </div>
    }
}

/// Table shell with consistent styling, horizontal scrolling and a caption for screen readers.
#[component]
pub fn DataTable(
    #[prop(into)] caption: String,
    headers: Vec<&'static str>,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="table-wrap">
            <table class="table">
                <caption class="visually-hidden">{caption}</caption>
                <thead>
                    <tr>{headers.into_iter().map(|h| view! { <th scope="col">{h}</th> }).collect_view()}</tr>
                </thead>
                <tbody>{children()}</tbody>
            </table>
        </div>
    }
}

/// A button that copies `text` to the clipboard.
#[component]
pub fn CopyButton(
    #[prop(into)] text: Signal<String>,
    #[prop(optional, into)] label: Option<String>,
) -> impl IntoView {
    let copied = RwSignal::new(false);
    let label = label.unwrap_or_else(|| "Copy".into());
    let on_click = move |_| {
        if let Some(window) = web_sys::window() {
            // Fire-and-forget: the promise only rejects without clipboard permission.
            let _ = window.navigator().clipboard().write_text(&text.get_untracked());
            copied.set(true);
            set_timeout(
                move || {
                    let _ = copied.try_set(false);
                },
                std::time::Duration::from_secs(2),
            );
        }
    };
    view! {
        <button type="button" class="btn btn-sm btn-secondary" on:click=on_click>
            <Show when=move || copied.get() fallback=|| view! { <Icon name="copy" /> }>
                <Icon name="check" />
            </Show>
            {move || if copied.get() { "Copied".to_string() } else { label.clone() }}
        </button>
    }
}
