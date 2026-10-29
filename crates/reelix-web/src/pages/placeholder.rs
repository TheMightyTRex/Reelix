//! "Coming in Mx" pages for navigation entries that later milestones implement, and 404.

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_location;

use crate::components::sidebar::app_path;
use crate::components::{EmptyState, Icon, PageHeader};

struct Planned {
    path: &'static str,
    title: &'static str,
    milestone: &'static str,
    icon: &'static str,
    blurb: &'static str,
}

const PLANNED: &[Planned] = &[
    Planned {
        path: "/settings/ui",
        title: "UI settings",
        milestone: "M2",
        icon: "settings",
        blurb: "Date formats, default views and calendar preferences.",
    },
    Planned {
        path: "/system/backup",
        title: "Backup",
        milestone: "M8",
        icon: "folder",
        blurb: "Scheduled backups and one-click restore.",
    },
    Planned {
        path: "/system/updates",
        title: "Updates",
        milestone: "M8",
        icon: "download",
        blurb: "Release notes and update checks.",
    },
    Planned {
        path: "/system/logs",
        title: "Logs",
        milestone: "M8",
        icon: "list",
        blurb: "Log viewer with level filters and live tail.",
    },
];

/// Catch-all route inside the shell: a planned screen, or a 404.
#[component]
pub fn PlaceholderPage() -> impl IntoView {
    let location = use_location();
    move || {
        let path = app_path(&location.pathname.get());
        match PLANNED.iter().find(|p| p.path == path) {
            Some(p) => view! {
                <PageHeader title=p.title />
                <div class="card coming-soon">
                    <EmptyState icon=p.icon title=format!("Coming in {}", p.milestone) description=p.blurb>
                        <span class="badge tone-accent">{format!("Milestone {}", p.milestone)}</span>
                    </EmptyState>
                </div>
            }
            .into_any(),
            None => view! { <NotFound /> }.into_any(),
        }
    }
}

#[component]
pub fn NotFound() -> impl IntoView {
    view! {
        <div class="card not-found">
            <EmptyState icon="discover" title="Page not found" description="There's nothing at this address.">
                <A href="/" attr:class="btn btn-primary">
                    <Icon name="dashboard" />
                    "Back home"
                </A>
            </EmptyState>
        </div>
    }
}
