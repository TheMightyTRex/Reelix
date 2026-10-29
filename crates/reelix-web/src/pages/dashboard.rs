//! `/` — admin dashboard.

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use reelix_api_types::library::RootFolderDto;
use reelix_api_types::providers::ProviderDto;
use reelix_api_types::requests::RequestCountsDto;
use reelix_api_types::{ApiKeyDto, HealthLevel, HealthReport};
use reelix_core::MediaKind;

use crate::api;
use crate::components::library_common::{on_entity_changed, pref_get, pref_set};
use crate::components::{Card, EmptyState, Icon, PageHeader, Skeleton, StatCard, StatusBadge, Tone};
use crate::pages::system::events::EventRow;
use crate::state::AuthState;
use crate::ws::LiveEvents;

/// UI preference set once the admin closes the "Getting started" card.
const GETTING_STARTED_DISMISSED: &str = "dashboard.getting_started_dismissed";

#[component]
pub fn DashboardPage() -> impl IntoView {
    let auth = AuthState::expect();
    let navigate = use_navigate();
    // The dashboard is the admin home; requesters start at Discover.
    Effect::new(move |_| {
        if auth.is_logged_in() && !auth.is_admin() {
            navigate("/discover", Default::default());
        }
    });

    let live = LiveEvents::expect();
    let getting_started = RwSignal::new(pref_get(GETTING_STARTED_DISMISSED).is_none());
    let dismiss_getting_started = move |_| {
        pref_set(GETTING_STARTED_DISMISSED, "1");
        getting_started.set(false);
    };
    let health = LocalResource::new(|| api::get::<HealthReport>("/system/health"));
    let keys = LocalResource::new(|| api::get::<Vec<ApiKeyDto>>("/auth/apikeys"));
    let roots = LocalResource::new(|| api::get::<Vec<RootFolderDto>>("/rootfolders"));
    let indexers = LocalResource::new(|| api::get::<Vec<ProviderDto>>("/indexers"));
    let clients = LocalResource::new(|| api::get::<Vec<ProviderDto>>("/downloadclients"));
    let servers = LocalResource::new(|| api::get::<Vec<ProviderDto>>("/mediaservers"));
    let counts = LocalResource::new(|| api::get::<RequestCountsDto>("/requests/count"));
    // Refresh the checklist and request tile when the relevant entities change.
    on_entity_changed(&["request"], false, move || counts.refetch());
    on_entity_changed(&["root_folder"], false, move || roots.refetch());
    on_entity_changed(
        &["indexer", "download_client", "media_server"],
        false,
        move || {
            indexers.refetch();
            clients.refetch();
            servers.refetch();
        },
    );

    let greeting = move || {
        let name = auth
            .user
            .with(|u| u.as_ref().map(|u| u.display_name.clone()).unwrap_or_default());
        format!("Welcome back, {name}")
    };

    let alerts = move || {
        let report = health.get()?.ok()?;
        let problems: Vec<_> = report
            .checks
            .into_iter()
            .filter(|c| c.level != HealthLevel::Ok)
            .collect();
        if problems.is_empty() {
            return None;
        }
        Some(view! {
            <div class="health-alerts">
                {problems
                    .into_iter()
                    .map(|c| {
                        let (class, icon) = if c.level == HealthLevel::Error {
                            ("alert alert-danger", "error")
                        } else {
                            ("alert alert-warning", "warning")
                        };
                        view! {
                            <div class=class role="alert">
                                <Icon name=icon />
                                <div class="alert-body">
                                    <strong>{c.source}</strong>
                                    <span>{c.message}</span>
                                </div>
                                <A href="/system/health" attr:class="alert-link">
                                    "Details"
                                </A>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
        })
    };

    let pending_requests = Signal::derive(move || {
        counts
            .get()
            .and_then(Result::ok)
            .map_or_else(|| "—".to_string(), |c| c.pending.to_string())
    });
    let queue_count = RwSignal::new(None::<u64>);
    let downloading = Signal::derive(move || {
        queue_count
            .get()
            .map_or_else(|| "—".to_string(), |n| n.to_string())
    });
    let has_keys = move || keys.get().and_then(Result::ok).is_some_and(|k| !k.is_empty());
    let has_root = move |kind: MediaKind| {
        roots
            .get()
            .and_then(Result::ok)
            .is_some_and(|r| r.iter().any(|r| r.media_kind == kind))
    };
    let any_enabled = |r: LocalResource<api::ApiResult<Vec<ProviderDto>>>| {
        r.get()
            .and_then(Result::ok)
            .is_some_and(|v| v.iter().any(|p| p.enabled))
    };

    let checklist = move || {
        let items: [(&str, Option<&str>, bool, Option<&str>); 7] = [
            ("Create the admin account", None, true, None),
            (
                "Add a root folder for movies",
                None,
                has_root(MediaKind::Movie),
                Some("/settings/mediamanagement"),
            ),
            (
                "Add a root folder for series",
                None,
                has_root(MediaKind::Series),
                Some("/settings/mediamanagement"),
            ),
            (
                "Add an indexer",
                None,
                any_enabled(indexers),
                Some("/settings/indexers"),
            ),
            (
                "Connect a download client",
                None,
                any_enabled(clients),
                Some("/settings/downloadclients"),
            ),
            (
                "Connect a media server",
                None,
                any_enabled(servers),
                Some("/settings/mediaservers"),
            ),
            (
                "Create an API key for companion apps",
                None,
                has_keys(),
                Some("/settings/general"),
            ),
        ];
        let done = items.iter().filter(|i| i.2).count();
        let total = items.len();
        view! {
            <div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax=total aria-valuenow=done
                aria-label="Setup progress">
                <div class="progress-bar" style=format!("width: {}%", done * 100 / total)></div>
            </div>
            <p class="checklist-summary">{format!("{done} of {total} done")}</p>
            <ul class="checklist">
                {items
                    .into_iter()
                    .map(|(label, milestone, done, href)| {
                        view! {
                            <li class="check-item" class:done=done class:locked=milestone.is_some()>
                                <span class="check-box" aria-hidden="true">
                                    {done.then(|| view! { <Icon name="check" /> })}
                                </span>
                                <span class="check-label">
                                    {match (href, done) {
                                        (Some(h), false) => view! { <A href=h>{label}</A> }.into_any(),
                                        _ => label.into_any(),
                                    }}
                                    <span class="visually-hidden">{if done { " (done)" } else { "" }}</span>
                                </span>
                                {milestone.map(|m| view! { <span class="badge tone-neutral">{format!("Coming in {m}")}</span> })}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
    };

    let activity = move || {
        let events = live.events.get();
        if events.is_empty() {
            view! {
                <EmptyState
                    icon="activity"
                    title="Nothing has happened yet"
                    description="Jobs, tasks and other server events appear here the moment they happen."
                    compact=true
                />
            }
            .into_any()
        } else {
            view! {
                <ul class="event-list">
                    {events.into_iter().take(8).map(|e| view! { <EventRow event=e /> }).collect_view()}
                </ul>
            }
            .into_any()
        }
    };

    let health_badge = move || match health.get() {
        None => view! { <StatusBadge tone=Tone::Neutral label="Checking…" /> }.into_any(),
        Some(Err(_)) => view! { <StatusBadge tone=Tone::Danger label="Unavailable" dot=true /> }.into_any(),
        Some(Ok(r)) => {
            let label = match r.status {
                HealthLevel::Ok => "All systems healthy",
                HealthLevel::Warning => "Needs attention",
                HealthLevel::Error => "Problems detected",
            };
            view! { <StatusBadge tone=Tone::health(r.status) label=label dot=true /> }.into_any()
        }
    };

    view! {
        <PageHeader title="Dashboard" subtitle=greeting()>
            {health_badge}
        </PageHeader>
        {alerts}
        <div class="stat-grid">
            <StatCard icon="download" label="Downloading" value=downloading hint="Items in the queue" tone=Tone::Info />
            <A href="/requests" attr:class="stat-link">
                <StatCard icon="requests" label="Pending requests" value=pending_requests hint="Awaiting approval" tone=Tone::Accent />
            </A>
            <crate::components::library_dash::LibraryStatTiles />
        </div>
        <div class="dash-grid">
            <Card
                title="Active downloads"
                class="span-2"
                actions=view! { <A href="/activity/queue" attr:class="card-link">"View queue"</A> }.into_any()
            >
                <crate::pages::activity::ActiveDownloads count=queue_count />
            </Card>
            <Show when=move || getting_started.get()>
                <Card
                    title="Getting started"
                    subtitle="Everything Reelix needs to automate your library."
                    actions=view! {
                        <button type="button" class="btn btn-sm btn-ghost" title="Close" on:click=dismiss_getting_started>
                            <Icon name="close" />
                            <span class="visually-hidden">"Close getting started"</span>
                        </button>
                    }
                    .into_any()
                >
                    {move || if keys.get().is_none() { view! { <Skeleton lines=4 /> }.into_any() } else { checklist().into_any() }}
                </Card>
            </Show>
            <Card title="Upcoming (7 days)">
                <crate::components::library_dash::UpcomingList />
            </Card>
            <Card title="Recently added">
                <crate::components::library_dash::RecentlyAdded />
            </Card>
            <Card
                title="Recent activity"
                subtitle="Live from the server"
                actions=view! { <A href="/system/events" attr:class="card-link">"View all"</A> }.into_any()
            >
                {activity}
            </Card>
        </div>
    }
}
