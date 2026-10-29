//! `/system/events` — recent jobs plus the live event stream.

use leptos::prelude::*;
use reelix_api_types::{DomainEvent, JobDto, JobStatus};

use super::require_admin;
use crate::api;
use crate::components::{
    Card, DataTable, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge, Tone,
};
use crate::format;
use crate::ws::{LiveEvent, LiveEvents, MAX_EVENTS};

/// Icon, tone and sentence describing an event.
pub fn describe(event: &DomainEvent) -> (&'static str, Tone, String) {
    match event {
        DomainEvent::SetupCompleted { .. } => ("sparkle", Tone::Accent, "Initial setup completed".into()),
        DomainEvent::UserCreated { username, .. } => {
            ("user", Tone::Accent, format!("User “{username}” created"))
        }
        DomainEvent::JobQueued { id, kind } => (
            "clock",
            Tone::Neutral,
            format!("{} queued (job #{id})", format::humanize_kind(kind)),
        ),
        DomainEvent::JobStarted { id, kind } => (
            "play",
            Tone::Info,
            format!("{} started (job #{id})", format::humanize_kind(kind)),
        ),
        DomainEvent::JobFinished {
            id,
            kind,
            status,
            duration_ms,
            error,
        } => {
            let name = format::humanize_kind(kind);
            let took = format::duration_ms(*duration_ms);
            match (status, error) {
                (JobStatus::Failed, Some(err)) => (
                    "error",
                    Tone::Danger,
                    format!("{name} failed after {took} (job #{id}): {err}"),
                ),
                (JobStatus::Failed, None) => (
                    "error",
                    Tone::Danger,
                    format!("{name} failed after {took} (job #{id})"),
                ),
                _ => (
                    "check",
                    Tone::Success,
                    format!("{name} finished in {took} (job #{id})"),
                ),
            }
        }
        DomainEvent::TaskUpdated { kind } => (
            "refresh",
            Tone::Neutral,
            format!("Task “{}” updated", format::humanize_kind(kind)),
        ),
        DomainEvent::EntityChanged {
            entity, action, id, ..
        } => (
            "refresh",
            Tone::Neutral,
            format!("{} #{id} {action}", format::humanize_kind(entity)),
        ),
    }
}

/// One line in an event feed.
#[component]
pub fn EventRow(event: LiveEvent) -> impl IntoView {
    let (icon, tone, text) = describe(&event.event);
    let when = event.received_at;
    view! {
        <li class="event-row">
            <span class=format!("event-icon {}", tone.class())>
                <Icon name=icon />
            </span>
            <span class="event-text">{text}</span>
            <time class="event-time" datetime=when.to_rfc3339() title=format::datetime(when)>
                {format::time(when)}
            </time>
        </li>
    }
}

fn is_job_event(e: &DomainEvent) -> bool {
    matches!(
        e,
        DomainEvent::JobQueued { .. } | DomainEvent::JobStarted { .. } | DomainEvent::JobFinished { .. }
    )
}

#[component]
pub fn EventsPage() -> impl IntoView {
    require_admin();
    let live = LiveEvents::expect();
    let jobs = LocalResource::new(|| api::get::<Vec<JobDto>>("/system/jobs?limit=50"));

    // Refresh the job table whenever a job changes state.
    Effect::new(move |prev: Option<u64>| {
        let latest = live.latest();
        let seq = latest.as_ref().map_or(0, |e| e.seq);
        if let (Some(prev), Some(ev)) = (prev, latest)
            && ev.seq != prev
            && is_job_event(&ev.event)
        {
            jobs.refetch();
        }
        seq
    });

    let job_table = move || {
        match jobs.get() {
        None => view! { <Skeleton lines=5 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| jobs.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => {
            view! { <EmptyState icon="list" title="No jobs yet" description="Background jobs appear here once they run." compact=true /> }
                .into_any()
        }
        Some(Ok(list)) => view! {
            <DataTable caption="Recent jobs" headers=vec!["#", "Job", "Status", "Attempts", "Queued", "Duration", "Error"]>
                {list
                    .into_iter()
                    .map(|job| {
                        let duration = match (job.started_at, job.finished_at) {
                            (Some(s), Some(f)) => format::duration_ms((f - s).num_milliseconds()),
                            (Some(_), None) => "running…".into(),
                            _ => "—".into(),
                        };
                        view! {
                            <tr>
                                <td class="mono muted">{job.id}</td>
                                <td class="cell-strong">{format::humanize_kind(&job.kind)}</td>
                                <td><StatusBadge tone=Tone::job(job.status) label=format::humanize_kind(job.status.as_str()) dot=true /></td>
                                <td>{job.attempts}</td>
                                <td title=format::datetime(job.created_at)>{format::relative(job.created_at)}</td>
                                <td>{duration}</td>
                                <td class="cell-error">{job.error.unwrap_or_default()}</td>
                            </tr>
                        }
                    })
                    .collect_view()}
            </DataTable>
        }
        .into_any(),
    }
    };

    let feed = move || {
        let events = live.events.get();
        if events.is_empty() {
            view! {
                <EmptyState icon="activity" title="Waiting for events" description="Live events stream in over the WebSocket while this page is open." compact=true />
            }
            .into_any()
        } else {
            view! { <ul class="event-list">{events.into_iter().map(|e| view! { <EventRow event=e /> }).collect_view()}</ul> }
                .into_any()
        }
    };

    view! {
        <PageHeader title="Events" subtitle="Background jobs and live server events.">
            <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| jobs.refetch()>
                <Icon name="refresh" />
                "Refresh"
            </button>
        </PageHeader>
        <div class="split-grid">
            <Card title="Recent jobs" subtitle="The last 50 jobs, newest first." class="span-2">
                {job_table}
            </Card>
            <Card title="Live feed" subtitle=format!("Last {MAX_EVENTS} events this session.")>
                {feed}
            </Card>
        </div>
    }
}
