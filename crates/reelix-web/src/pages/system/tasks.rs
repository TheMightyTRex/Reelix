//! `/system/tasks` — scheduled tasks with "Run now".

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::{DomainEvent, EnqueuedJob, TaskDto};

use super::require_admin;
use crate::api;
use crate::components::{
    Card, DataTable, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge, Tone,
};
use crate::format;
use crate::state::Toasts;
use crate::ws::LiveEvents;

#[component]
pub fn TasksPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let live = LiveEvents::expect();
    let tasks = LocalResource::new(|| api::get::<Vec<TaskDto>>("/system/tasks"));
    // Kinds with a "Run now" request in flight.
    let starting = RwSignal::new(Vec::<String>::new());

    Effect::new(move |prev: Option<u64>| {
        let latest = live.latest();
        let seq = latest.as_ref().map_or(0, |e| e.seq);
        if let (Some(prev), Some(ev)) = (prev, latest)
            && ev.seq != prev
            && matches!(
                ev.event,
                DomainEvent::TaskUpdated { .. }
                    | DomainEvent::JobFinished { .. }
                    | DomainEvent::JobStarted { .. }
            )
        {
            tasks.refetch();
        }
        seq
    });

    let run_now = move |kind: String, name: String| {
        starting.update(|s| s.push(kind.clone()));
        spawn_local(async move {
            match api::post_empty::<EnqueuedJob>(&format!("/system/tasks/{kind}/run")).await {
                Ok(EnqueuedJob { job_id: Some(id) }) => {
                    toasts.success(format!("{name} queued as job #{id}."))
                }
                Ok(EnqueuedJob { job_id: None }) => {
                    toasts.info(format!("{name} is already queued or running."))
                }
                Err(e) => toasts.error(format!("Could not start {name}: {e}")),
            }
            starting.update(|s| s.retain(|k| k != &kind));
            tasks.refetch();
        });
    };

    let table = move || {
        match tasks.get() {
        None => view! { <Skeleton lines=5 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| tasks.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => {
            view! { <EmptyState icon="clock" title="No scheduled tasks" compact=true /> }.into_any()
        }
        Some(Ok(list)) => view! {
            <DataTable caption="Scheduled tasks" headers=vec!["Task", "Interval", "Last run", "Duration", "Next run", "Status", ""]>
                {list
                    .into_iter()
                    .map(|task| {
                        let kind = task.kind.clone();
                        let name = task.name.clone();
                        let busy_kind = task.kind.clone();
                        let running = task.running;
                        let busy = Signal::derive(move || running || starting.with(|s| s.contains(&busy_kind)));
                        let status = if task.running {
                            view! { <StatusBadge tone=Tone::Info label="Running" dot=true /> }.into_any()
                        } else if !task.enabled {
                            view! { <StatusBadge tone=Tone::Neutral label="Disabled" /> }.into_any()
                        } else if let Some(err) = task.last_error.clone() {
                            view! { <span title=err><StatusBadge tone=Tone::Danger label="Last run failed" dot=true /></span> }.into_any()
                        } else if task.last_run.is_some() {
                            view! { <StatusBadge tone=Tone::Success label="OK" dot=true /> }.into_any()
                        } else {
                            view! { <StatusBadge tone=Tone::Neutral label="Never run" /> }.into_any()
                        };
                        view! {
                            <tr>
                                <td>
                                    <span class="cell-strong">{task.name.clone()}</span>
                                    <span class="cell-sub mono">{task.kind.clone()}</span>
                                </td>
                                <td>{format::interval(task.interval_secs)}</td>
                                <td title=task.last_run.map(format::datetime).unwrap_or_default()>
                                    {task.last_run.map(format::relative).unwrap_or_else(|| "Never".into())}
                                </td>
                                <td>{task.last_duration_ms.map(format::duration_ms).unwrap_or_else(|| "—".into())}</td>
                                <td title=task.next_run.map(format::datetime).unwrap_or_default()>
                                    {task.next_run.map(format::relative).unwrap_or_else(|| "—".into())}
                                </td>
                                <td>{status}</td>
                                <td class="cell-actions">
                                    <button
                                        type="button"
                                        class="btn btn-sm btn-secondary"
                                        disabled=busy
                                        aria-label=format!("Run {} now", task.name)
                                        on:click=move |_| run_now(kind.clone(), name.clone())
                                    >
                                        <Icon name="play" />
                                        {move || if busy.get() { "Running…" } else { "Run now" }}
                                    </button>
                                </td>
                            </tr>
                        }
                    })
                    .collect_view()}
            </DataTable>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="Tasks" subtitle="Scheduled maintenance that Reelix runs in the background.">
            <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| tasks.refetch()>
                <Icon name="refresh" />
                "Refresh"
            </button>
        </PageHeader>
        <Card>{table}</Card>
    }
}
