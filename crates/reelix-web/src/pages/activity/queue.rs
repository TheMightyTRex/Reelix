//! `/activity/queue` — live download progress across every client, pending (delayed) releases,
//! and the queue actions: remove (optionally from the client / blocklist), manual import,
//! retry import and ignore. Also the dashboard's "Active downloads" panel. `VIEW_QUEUE` without
//! `MANAGE_LIBRARY` gets a read-only queue (no pending releases, no actions).

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::Permissions;
use reelix_api_types::pipeline::{
    Paged, PendingReleaseDto, QueueBulkRemove, QueueBulkRemoveResult, QueueItemDto,
};

use crate::api;
use crate::components::history_table::{FormatScore, MediaCell, ProtoBadge, media_title, quality_label};
use crate::components::library_common::{Pager, bytes, on_entity_changed, toggle_id};
use crate::components::library_forms::{require_any, with_query};
use crate::components::manual_import::{ManualImportModal, ManualImportSource};
use crate::components::{
    Card, ConfirmDialog, EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton, StatusBadge, Tone,
};
use crate::format;
use crate::state::{AuthState, Toasts};

const PAGE_SIZE: u32 = 50;
/// Fallback refresh while the queue is on screen (progress also arrives via `entity.changed`).
const POLL: Duration = Duration::from_secs(10);

/// Label + tone of a tracked state.
fn state_badge(state: &str) -> (Tone, &'static str) {
    match state {
        "downloading" => (Tone::Info, "Downloading"),
        "import_pending" => (Tone::Warning, "Import pending"),
        "importing" => (Tone::Accent, "Importing"),
        "failed_pending" => (Tone::Danger, "Failed"),
        "imported" => (Tone::Success, "Imported"),
        "failed" => (Tone::Danger, "Failed"),
        "ignored" => (Tone::Neutral, "Ignored"),
        _ => (Tone::Neutral, "Unknown"),
    }
}

/// Label + tone of a client state.
fn client_badge(state: &str) -> (Tone, String) {
    let tone = match state {
        "downloading" => Tone::Info,
        "completed" => Tone::Success,
        "failed" => Tone::Danger,
        "warning" => Tone::Warning,
        "post_processing" => Tone::Accent,
        _ => Tone::Neutral,
    };
    (tone, format::humanize_kind(state))
}

/// Runs `f` every `every` while the calling component is alive.
fn poll(every: Duration, f: impl Fn() + 'static) {
    let handle = set_interval_with_handle(f, every).ok();
    on_cleanup(move || {
        if let Some(h) = handle {
            h.clear();
        }
    });
}

/// Progress bar with percentage label.
#[component]
fn ProgressCell(progress: f64, #[prop(optional)] failed: bool) -> impl IntoView {
    let pct = (progress.clamp(0.0, 1.0) * 100.0).round() as u32;
    view! {
        <div class="q-progress" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow=pct
            aria-label="Download progress">
            <div class="progress">
                <div class="progress-bar" class:full=failed style=format!("width: {pct}%")></div>
            </div>
            <span class="q-pct">{format!("{pct}%")}</span>
        </div>
    }
}

/// Options for removing a queue item.
#[derive(Clone, Copy)]
struct RemoveOpts {
    from_client: RwSignal<bool>,
    blocklist: RwSignal<bool>,
    skip_redownload: RwSignal<bool>,
}

#[component]
pub fn QueuePage() -> impl IntoView {
    require_any(Permissions::MANAGE_LIBRARY | Permissions::VIEW_QUEUE);
    let manage = AuthState::expect().has(Permissions::MANAGE_LIBRARY);
    let toasts = Toasts::expect();
    let page = RwSignal::new(1u32);
    let queue = LocalResource::new(move || {
        let url = with_query(
            "/queue",
            &[
                ("page", page.get().to_string()),
                ("page_size", PAGE_SIZE.to_string()),
            ],
        );
        async move { api::get::<Paged<QueueItemDto>>(&url).await }
    });
    let pending = LocalResource::new(move || async move {
        if manage {
            api::get::<Vec<PendingReleaseDto>>("/pending").await
        } else {
            Ok(Vec::new())
        }
    });
    on_entity_changed(&["queue"], false, move || queue.refetch());
    on_entity_changed(&["pending"], false, move || pending.refetch());
    poll(POLL, move || queue.refetch());
    let total = Signal::derive(move || queue.get().and_then(Result::ok).map_or(0, |p| p.total));

    let remove_target = RwSignal::new(None::<QueueItemDto>);
    let remove_open = RwSignal::new(false);
    // Bulk removal: the selected ids, and whether the remove dialog is acting on them.
    let selected = RwSignal::new(Vec::<i64>::new());
    let bulk = RwSignal::new(false);
    let page_ids = Signal::derive(move || {
        queue
            .get()
            .and_then(Result::ok)
            .map(|p| p.items.iter().map(|q| q.id).collect::<Vec<_>>())
            .unwrap_or_default()
    });
    let all_selected = move || {
        let ids = page_ids.get();
        !ids.is_empty() && selected.with(|s| ids.iter().all(|id| s.contains(id)))
    };
    let toggle_all = move |on: bool| {
        let ids = page_ids.get_untracked();
        selected.update(|s| {
            s.retain(|id| !ids.contains(id));
            if on {
                s.extend(ids);
            }
        });
    };
    Effect::new(move |_| {
        page.track();
        selected.set(Vec::new());
    });
    let opts = RemoveOpts {
        from_client: RwSignal::new(true),
        blocklist: RwSignal::new(false),
        skip_redownload: RwSignal::new(false),
    };
    let mi_open = RwSignal::new(false);
    let mi_source = RwSignal::new(None::<ManualImportSource>);
    let pending_delete = RwSignal::new(None::<PendingReleaseDto>);
    let pending_delete_open = RwSignal::new(false);

    let open_remove = move |item: QueueItemDto| {
        opts.from_client.set(true);
        opts.blocklist.set(false);
        opts.skip_redownload.set(false);
        remove_target.set(Some(item));
        bulk.set(false);
        remove_open.set(true);
    };
    // Off by default: stale entries are often other tools' finished downloads.
    let open_bulk_remove = move |_| {
        opts.from_client.set(false);
        opts.blocklist.set(false);
        opts.skip_redownload.set(false);
        remove_target.set(None);
        bulk.set(true);
        remove_open.set(true);
    };
    let do_bulk_remove = move || {
        let blocklist = opts.blocklist.get_untracked();
        let body = QueueBulkRemove {
            ids: selected.get_untracked(),
            remove_from_client: opts.from_client.get_untracked(),
            blocklist,
            skip_redownload: blocklist && opts.skip_redownload.get_untracked(),
        };
        remove_open.set(false);
        spawn_local(async move {
            match api::delete_with_body::<_, QueueBulkRemoveResult>("/queue/bulk", &body).await {
                Ok(r) => {
                    let plural = if r.removed == 1 { "" } else { "s" };
                    if r.failed.is_empty() {
                        toasts.success(format!("Removed {} item{plural}.", r.removed));
                    } else {
                        toasts.error(format!(
                            "Removed {} item{plural}; {} could not be removed: {}",
                            r.removed,
                            r.failed.len(),
                            r.failed[0].error
                        ));
                    }
                    selected.update(|s| s.retain(|id| r.failed.iter().any(|f| f.id == *id)));
                    queue.refetch();
                }
                Err(e) => toasts.error(format!("Could not remove: {e}")),
            }
        });
    };
    let do_remove = move |_| {
        if bulk.get_untracked() {
            do_bulk_remove();
            return;
        }
        let Some(item) = remove_target.get_untracked() else {
            return;
        };
        let blocklist = opts.blocklist.get_untracked();
        let url = with_query(
            &format!("/queue/{}", item.id),
            &[
                ("remove_from_client", opts.from_client.get_untracked().to_string()),
                ("blocklist", blocklist.to_string()),
                (
                    "skip_redownload",
                    (blocklist && opts.skip_redownload.get_untracked()).to_string(),
                ),
            ],
        );
        remove_open.set(false);
        spawn_local(async move {
            match api::delete(&url).await {
                Ok(()) => {
                    toasts.success(if blocklist {
                        format!("Removed and blocklisted “{}”.", item.title)
                    } else {
                        format!("Removed “{}”.", item.title)
                    });
                    queue.refetch();
                }
                Err(e) => toasts.error(format!("Could not remove: {e}")),
            }
        });
    };
    let action = move |path: String, ok: &'static str| {
        spawn_local(async move {
            match api::post_empty::<serde_json::Value>(&path).await {
                Ok(_) => {
                    toasts.success(ok);
                    queue.refetch();
                }
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };
    let open_manual = move |item: Option<QueueItemDto>| {
        mi_source.set(Some(match item {
            Some(i) => ManualImportSource {
                folder: i.output_path.clone(),
                download_id: Some(i.download_id.clone()),
                movie_id: i.media.as_ref().filter(|m| m.kind == "movie").map(|m| m.id),
                series_id: i.media.as_ref().filter(|m| m.kind == "series").map(|m| m.id),
                label: Some(i.title.clone()),
            },
            None => ManualImportSource::default(),
        }));
        mi_open.set(true);
    };
    let pending_action = move |id: i64, grab: bool| {
        spawn_local(async move {
            let res = if grab {
                api::post_empty::<serde_json::Value>(&format!("/pending/{id}/grab"))
                    .await
                    .map(|_| ())
            } else {
                api::delete(&format!("/pending/{id}")).await
            };
            match res {
                Ok(()) => {
                    toasts.success(if grab {
                        "Grabbed."
                    } else {
                        "Pending release removed."
                    });
                    pending.refetch();
                    if grab {
                        queue.refetch();
                    }
                }
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };

    let queue_body = move || {
        match queue.get() {
        None => view! { <Skeleton lines=5 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| queue.refetch()) /> }.into_any(),
        Some(Ok(p)) if p.items.is_empty() => view! {
            <EmptyState icon="download" title="Nothing is downloading"
                description="Grabbed releases show up here with live progress until they're imported.">
                {manage.then(|| view! { <A href="/wanted/missing" attr:class="btn btn-secondary">"Find missing"</A> })}
            </EmptyState>
        }
        .into_any(),
        Some(Ok(p)) => view! {
            <div class="table-wrap">
                <table class="table queue-table">
                    <caption class="visually-hidden">"Download queue"</caption>
                    <thead>
                        <tr>
                            {manage.then(|| view! {
                                <th scope="col" class="col-check">
                                    <input type="checkbox" aria-label="Select all on this page" prop:checked=all_selected
                                        on:change=move |e| toggle_all(event_target_checked(&e)) />
                                </th>
                            })}
                            <th scope="col">"Media"</th>
                            <th scope="col">"Release"</th>
                            <th scope="col">"Quality"</th>
                            <th scope="col">"Status"</th>
                            <th scope="col">"Progress"</th>
                            <th scope="col">"Size"</th>
                            <th scope="col">"ETA"</th>
                            {manage.then(|| view! { <th scope="col"><span class="visually-hidden">"Actions"</span></th> })}
                        </tr>
                    </thead>
                    <tbody>
                        {p.items.into_iter().map(|q| {
                            let (tone, label) = state_badge(&q.state);
                            let client = q.client_state.as_deref().map(client_badge);
                            let failed = q.state.starts_with("failed") || q.client_state.as_deref() == Some("failed");
                            let messages = q.messages.join("\n");
                            let has_messages = !q.messages.is_empty();
                            let retryable = matches!(q.state.as_str(), "import_pending" | "failed_pending");
                            let size = match (q.size, q.remaining) {
                                (Some(s), Some(r)) if r > 0 => format!("{} / {}", bytes(s.saturating_sub(r)), bytes(s)),
                                (Some(s), _) => bytes(s),
                                _ => "—".into(),
                            };
                            let id = q.id;
                            let q_rm = q.clone();
                            let q_mi = q.clone();
                            let is_sel = move || selected.with(|s| s.contains(&id));
                            let select_label = format!("Select {}", q.title);
                            view! {
                                <tr class:q-failed=failed class:is-selected=is_sel>
                                    {manage.then(|| view! {
                                        <td class="col-check">
                                            <input type="checkbox" aria-label=select_label prop:checked=is_sel
                                                on:change=move |_| toggle_id(selected, id) />
                                        </td>
                                    })}
                                    <td><MediaCell media=q.media.clone() episodes=q.episodes.clone() /></td>
                                    <td class="release-title">
                                        <ProtoBadge protocol=q.protocol />
                                        " "
                                        <span class="mono">{q.title.clone()}</span>
                                        <span class="cell-sub">
                                            {q.download_client.clone()}
                                            {q.indexer.clone().map(|i| format!(" · {i}"))}
                                            {format!(" · added {}", format::relative(q.added))}
                                        </span>
                                        {has_messages.then(|| view! {
                                            <ul class="reject-list q-messages">
                                                {q.messages.iter().map(|m| view! { <li>{m.clone()}</li> }).collect_view()}
                                            </ul>
                                        })}
                                    </td>
                                    <td>
                                        {q.quality.as_ref().map(|qm| view! { <span class="badge tone-info">{quality_label(qm)}</span> })}
                                        {(!q.custom_formats.is_empty() || q.format_score != 0).then(|| view! {
                                            <span class="cell-sub"><FormatScore score=q.format_score formats=q.custom_formats.clone() /></span>
                                        })}
                                    </td>
                                    <td>
                                        <span class="q-badges" title=messages.clone()>
                                            <StatusBadge tone=tone label=label dot=true />
                                            {client.map(|(t, l)| view! { <StatusBadge tone=t label=l /> })}
                                            {has_messages.then(|| view! {
                                                <span class="q-warn" aria-label=format!("Messages: {messages}")><Icon name="warning" /></span>
                                            })}
                                        </span>
                                    </td>
                                    <td><ProgressCell progress=q.progress failed=failed /></td>
                                    <td class="nowrap">{size}</td>
                                    <td class="nowrap">{q.eta_secs.filter(|_| q.state == "downloading").map(format::duration_secs).unwrap_or_else(|| "—".into())}</td>
                                    {manage.then(|| view! {
                                    <td class="cell-actions">
                                        {q.can_manual_import.then(|| view! {
                                            <button type="button" class="btn btn-sm btn-ghost" title="Manual import"
                                                on:click=move |_| open_manual(Some(q_mi.clone()))>
                                                <Icon name="folder" />
                                                <span class="visually-hidden">"Manual import"</span>
                                            </button>
                                        })}
                                        {retryable.then(|| view! {
                                            <button type="button" class="btn btn-sm btn-ghost" title="Retry import"
                                                on:click=move |_| action(format!("/queue/{id}/import"), "Import retry queued.")>
                                                <Icon name="refresh" />
                                                <span class="visually-hidden">"Retry import"</span>
                                            </button>
                                            <button type="button" class="btn btn-sm btn-ghost" title="Ignore (stop tracking, keep in client)"
                                                on:click=move |_| action(format!("/queue/{id}/ignore"), "Download ignored.")>
                                                <Icon name="close" />
                                                <span class="visually-hidden">"Ignore"</span>
                                            </button>
                                        })}
                                        <button type="button" class="btn btn-sm btn-ghost btn-danger-text" title="Remove"
                                            on:click=move |_| open_remove(q_rm.clone())>
                                            <Icon name="trash" />
                                            <span class="visually-hidden">"Remove"</span>
                                        </button>
                                    </td>
                                    })}
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <Pager page=page page_size=PAGE_SIZE total=total />
        }
        .into_any(),
    }
    };

    let pending_body = move || {
        let list = match pending.get() {
            Some(Ok(l)) if !l.is_empty() => l,
            _ => return None,
        };
        Some(view! {
            <Card title="Pending releases" subtitle="Held back by a delay profile; grabbed automatically when the delay ends.">
                <div class="table-wrap">
                    <table class="table">
                        <caption class="visually-hidden">"Pending releases"</caption>
                        <thead>
                            <tr>
                                <th scope="col">"Media"</th>
                                <th scope="col">"Release"</th>
                                <th scope="col">"Quality"</th>
                                <th scope="col">"Reason"</th>
                                <th scope="col">"Releases"</th>
                                <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                            </tr>
                        </thead>
                        <tbody>
                            {list.into_iter().map(|p| {
                                let id = p.id;
                                let p2 = p.clone();
                                let reason = if p.reason == "preferred_protocol" { "Preferred protocol" } else { "Delay profile" };
                                view! {
                                    <tr>
                                        <td><MediaCell media=p.media.clone() episodes=p.episodes.clone() /></td>
                                        <td class="release-title">
                                            <ProtoBadge protocol=p.protocol />
                                            " "
                                            <span class="mono">{p.title.clone()}</span>
                                            <span class="cell-sub">{p.indexer.clone()}</span>
                                        </td>
                                        <td><span class="badge tone-info">{quality_label(&p.quality)}</span></td>
                                        <td>{reason}</td>
                                        <td title=format::datetime(p.release_at)>{format::relative(p.release_at)}</td>
                                        <td class="cell-actions">
                                            <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| pending_action(id, true)>
                                                <Icon name="download" />
                                                "Grab now"
                                            </button>
                                            <button type="button" class="btn btn-sm btn-ghost btn-danger-text" title="Remove"
                                                on:click=move |_| { pending_delete.set(Some(p2.clone())); pending_delete_open.set(true); }>
                                                <Icon name="trash" />
                                                <span class="visually-hidden">"Remove"</span>
                                            </button>
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            </Card>
        })
    };

    let remove_title = move || {
        if bulk.get() {
            let n = selected.with(Vec::len);
            return format!("{n} selected item{}", if n == 1 { "" } else { "s" });
        }
        remove_target
            .get()
            .map(|q| match &q.media {
                Some(m) => format!("“{}” ({})", q.title, media_title(m)),
                None => format!("“{}”", q.title),
            })
            .unwrap_or_default()
    };

    view! {
        <PageHeader title="Queue" subtitle="Live download progress across every client.">
            {move || (manage && !selected.with(Vec::is_empty)).then(|| view! {
                <button type="button" class="btn btn-danger" on:click=open_bulk_remove>
                    <Icon name="trash" />
                    {move || format!("Remove selected ({})", selected.with(Vec::len))}
                </button>
            })}
            {manage.then(|| view! {
                <button type="button" class="btn btn-secondary" on:click=move |_| open_manual(None)>
                    <Icon name="folder" />
                    "Manual import"
                </button>
            })}
            <button type="button" class="btn btn-ghost" on:click=move |_| { queue.refetch(); pending.refetch(); }>
                <Icon name="refresh" />
                "Refresh"
            </button>
        </PageHeader>
        <div class="card">
            <div class="card-body">{queue_body}</div>
        </div>
        {pending_body}
        <Modal open=remove_open title="Remove from queue">
            <div class="form">
                <p class="modal-text">{remove_title}</p>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || opts.from_client.get()
                        on:change=move |e| opts.from_client.set(event_target_checked(&e)) />
                    "Remove from download client (deletes the download and its data)"
                </label>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || opts.blocklist.get()
                        on:change=move |e| opts.blocklist.set(event_target_checked(&e)) />
                    "Blocklist the release (and search for a replacement)"
                </label>
                <label class="checkbox" class:muted=move || !opts.blocklist.get()>
                    <input type="checkbox" disabled=move || !opts.blocklist.get() prop:checked=move || opts.skip_redownload.get()
                        on:change=move |e| opts.skip_redownload.set(event_target_checked(&e)) />
                    "Don't search for a replacement"
                </label>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| remove_open.set(false)>"Cancel"</button>
                    <button type="button" class="btn btn-danger" on:click=do_remove>"Remove"</button>
                </div>
            </div>
        </Modal>
        <ManualImportModal open=mi_open source=mi_source />
        <ConfirmDialog open=pending_delete_open title="Remove pending release?"
            message=Signal::derive(move || pending_delete.get().map(|p| format!("“{}” won't be grabbed when its delay ends.", p.title)).unwrap_or_default())
            confirm_label="Remove"
            on_confirm=Callback::new(move |()| { if let Some(p) = pending_delete.get_untracked() { pending_action(p.id, false); } }) />
    }
}

/// Dashboard panel: the first few active downloads with progress, plus the queue size.
#[component]
pub fn ActiveDownloads(
    /// Receives the queue total (for the "Downloading" stat tile).
    count: RwSignal<Option<u64>>,
) -> impl IntoView {
    let queue = LocalResource::new(|| api::get::<Paged<QueueItemDto>>("/queue?page=1&page_size=6"));
    on_entity_changed(&["queue"], false, move || queue.refetch());
    poll(Duration::from_secs(15), move || queue.refetch());
    Effect::new(move |_| {
        if let Some(r) = queue.get() {
            count.set(r.ok().map(|p| p.total));
        }
    });
    move || match queue.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(_)) => view! {
            <EmptyState icon="download" title="Queue unavailable" compact=true
                description="The download queue couldn't be loaded." />
        }
        .into_any(),
        Some(Ok(p)) if p.items.is_empty() => view! {
            <EmptyState icon="download" title="No active downloads" compact=true
                description="Grabbed releases show live progress here." />
        }
        .into_any(),
        Some(Ok(p)) => {
            let more = p.total.saturating_sub(p.items.len() as u64);
            view! {
                <ul class="dash-downloads">
                    {p.items.into_iter().map(|q| {
                        let name = q.media.as_ref().map(media_title).unwrap_or_else(|| q.title.clone());
                        let eps = crate::components::history_table::episodes_label(&q.episodes);
                        let failed = q.state.starts_with("failed");
                        view! {
                            <li class="dash-download">
                                <div class="dash-download-head">
                                    <span class="cell-strong">{name}{eps.map(|e| format!(" {e}"))}</span>
                                    <span class="muted">
                                        {q.quality.as_ref().map(quality_label)}
                                        {q.eta_secs.filter(|_| q.state == "downloading").map(|s| format!(" · {}", format::duration_secs(s)))}
                                        {format!(" · {}", q.download_client)}
                                    </span>
                                </div>
                                <ProgressCell progress=q.progress failed=failed />
                            </li>
                        }
                    }).collect_view()}
                </ul>
                {(more > 0).then(|| view! { <p class="muted">{format!("and {more} more")}</p> })}
            }
            .into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_states() {
        assert_eq!(state_badge("import_pending").1, "Import pending");
        assert_eq!(client_badge("post_processing").1, "Post processing");
    }
}
