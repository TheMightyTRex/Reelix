//! `/system/logs` — log viewer with level and text filters, a live tail and file downloads.

use leptos::prelude::*;
use reelix_api_types::system::{LogEntryDto, LogFileDto, LogLevel, LogsDto};

use super::require_admin;
use crate::api;
use crate::components::library_common::{Segmented, bytes};
use crate::components::{Card, DataTable, EmptyState, ErrorBox, Icon, PageHeader, Skeleton};
use crate::format;

/// How often the live tail refreshes.
const TAIL_MS: u32 = 3000;

fn level_class(l: Option<LogLevel>) -> &'static str {
    match l {
        Some(LogLevel::Error) => "log-error",
        Some(LogLevel::Warn) => "log-warn",
        Some(LogLevel::Info) => "log-info",
        Some(LogLevel::Debug) => "log-debug",
        Some(LogLevel::Trace) => "log-trace",
        None => "",
    }
}

fn level_label(l: Option<LogLevel>) -> &'static str {
    match l {
        Some(LogLevel::Error) => "ERROR",
        Some(LogLevel::Warn) => "WARN",
        Some(LogLevel::Info) => "INFO",
        Some(LogLevel::Debug) => "DEBUG",
        Some(LogLevel::Trace) => "TRACE",
        None => "",
    }
}

fn query(file: &str, level: &str, search: &str, limit: &str) -> String {
    let mut q = format!("/system/logs?level={level}&limit={limit}");
    if !file.is_empty() {
        q.push_str(&format!("&file={file}"));
    }
    let search = search.trim();
    if !search.is_empty() {
        q.push_str(&format!("&search={}", encode_component(search)));
    }
    q
}

/// Percent-encodes a query value like `encodeURIComponent`.
fn encode_component(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[component]
pub fn LogsPage() -> impl IntoView {
    require_admin();
    let file = RwSignal::new(String::new());
    let level = RwSignal::new("info".to_string());
    let search = RwSignal::new(String::new());
    // Debounced copy of `search`, so typing doesn't fire a request per key.
    let applied_search = RwSignal::new(String::new());
    let limit = RwSignal::new("500".to_string());
    let live = RwSignal::new(false);

    let files = LocalResource::new(|| api::get::<Vec<LogFileDto>>("/system/logs/files"));
    let logs = LocalResource::new(move || {
        let q = query(&file.get(), &level.get(), &applied_search.get(), &limit.get());
        async move { api::get::<LogsDto>(&q).await }
    });

    // Apply the search box 400 ms after the last keystroke.
    Effect::new(move |_| {
        let text = search.get();
        leptos::task::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(400).await;
            if search.get_untracked() == text && applied_search.get_untracked() != text {
                applied_search.set(text);
            }
        });
    });

    // Live tail: refetch on a timer while switched on. Each start bumps the generation, so a
    // loop from an earlier toggle (or after leaving the page) stops instead of doubling up.
    let generation = StoredValue::new(0u32);
    on_cleanup(move || generation.update_value(|g| *g += 1));
    Effect::new(move |_| {
        let on = live.get();
        generation.update_value(|g| *g += 1);
        if !on {
            return;
        }
        let mine = generation.get_value();
        leptos::task::spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(TAIL_MS).await;
                if generation.try_get_value() != Some(mine) {
                    break;
                }
                logs.refetch();
            }
        });
    });

    let file_options = move || {
        files
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, f)| {
                let label = if i == 0 {
                    format!("{} (current)", f.name)
                } else {
                    f.name.clone()
                };
                // The newest file is also what an empty selection shows.
                let value = if i == 0 { String::new() } else { f.name };
                view! { <option value=value>{label}</option> }
            })
            .collect_view()
    };

    let entries = move || {
        match logs.get() {
        None => view! { <Skeleton lines=10 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| logs.refetch()) /> }.into_any(),
        Some(Ok(l)) if l.file.is_none() => {
            view! { <EmptyState icon="list" title="No log files yet" compact=true /> }.into_any()
        }
        Some(Ok(l)) if l.entries.is_empty() => view! {
            <EmptyState icon="search" title="No matching entries" compact=true
                description="Lower the level or clear the search to see more." />
        }
        .into_any(),
        Some(Ok(l)) => view! {
            {l.truncated.then(|| view! {
                <p class="field-hint">"This file is large, so only its most recent part is shown. Download it for the full log."</p>
            })}
            <ol class="log-list" aria-label="Log entries, newest first">
                {l.entries.into_iter().map(|e| view! { <Entry e=e /> }).collect_view()}
            </ol>
        }
        .into_any(),
    }
    };

    let file_table = move || match files.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => {
            view! { <EmptyState icon="list" title="No log files yet" compact=true /> }.into_any()
        }
        Some(Ok(list)) => view! {
            <DataTable caption="Log files" headers=vec!["File", "Modified", "Size", ""]>
                {list
                    .into_iter()
                    .map(|f| {
                        let href = api::api_url(&format!("/system/logs/files/{}", f.name));
                        view! {
                            <tr>
                                <td class="mono">{f.name.clone()}</td>
                                <td title=format::datetime(f.modified)>{format::relative(f.modified)}</td>
                                <td>{bytes(f.size_bytes)}</td>
                                <td class="cell-actions">
                                    <a class="btn btn-sm btn-secondary" href=href download=f.name.clone()
                                        aria-label=format!("Download {}", f.name)>
                                        <Icon name="download" />
                                        "Download"
                                    </a>
                                </td>
                            </tr>
                        }
                    })
                    .collect_view()}
            </DataTable>
        }
        .into_any(),
    };

    view! {
        <PageHeader title="Logs" subtitle="What Reelix has been doing, newest first. Logs are kept for 14 days.">
            <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| {
                files.refetch();
                logs.refetch();
            }>
                <Icon name="refresh" />
                "Refresh"
            </button>
        </PageHeader>
        <div class="stack">
            <Card>
                <div class="toolbar log-toolbar">
                    <label class="visually-hidden" for="log-file">"Log file"</label>
                    <select id="log-file" class="input" prop:value=move || file.get()
                        on:change=move |e| file.set(event_target_value(&e))>
                        {file_options}
                    </select>
                    <Segmented value=level label="Minimum level" options=vec![
                        ("error", "Errors"),
                        ("warn", "Warnings"),
                        ("info", "Info"),
                        ("debug", "Debug"),
                        ("trace", "All"),
                    ] />
                    <label class="visually-hidden" for="log-search">"Search"</label>
                    <input id="log-search" class="input" type="search" placeholder="Search messages…"
                        prop:value=move || search.get() on:input=move |e| search.set(event_target_value(&e)) />
                    <label class="visually-hidden" for="log-limit">"Entries"</label>
                    <select id="log-limit" class="input" prop:value=move || limit.get()
                        on:change=move |e| limit.set(event_target_value(&e))>
                        <option value="200">"200 entries"</option>
                        <option value="500">"500 entries"</option>
                        <option value="2000">"2,000 entries"</option>
                        <option value="5000">"5,000 entries"</option>
                    </select>
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || live.get()
                            on:change=move |e| live.set(event_target_checked(&e)) />
                        "Live"
                    </label>
                </div>
                <p class="visually-hidden" aria-live="polite">
                    {move || if live.get() { "Live tail on" } else { "" }}
                </p>
                {entries}
            </Card>
            <Card title="Files">{file_table}</Card>
        </div>
    }
}

#[component]
fn Entry(e: LogEntryDto) -> impl IntoView {
    let class = format!("log-entry {}", level_class(e.level));
    view! {
        <li class=class>
            <span class="log-time" title=e.time.map(format::datetime).unwrap_or_default()>
                {e.time.map(format::time).unwrap_or_default()}
            </span>
            <span class="log-level">{level_label(e.level)}</span>
            <span class="log-body">
                {e.target.map(|t| view! { <span class="log-target">{t}</span> })}
                <span class="log-message">{e.message}</span>
            </span>
        </li>
    }
}
