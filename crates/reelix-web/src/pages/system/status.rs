//! `/system/status` — version and runtime information.

use leptos::prelude::*;
use reelix_api_types::SystemStatus;

use super::require_admin;
use crate::api;
use crate::components::{Card, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge, Tone};
use crate::format;
use crate::ws::{LiveEvents, WsStatus};

#[component]
pub fn StatusPage() -> impl IntoView {
    require_admin();
    let live = LiveEvents::expect();
    let status = LocalResource::new(|| api::get::<SystemStatus>("/system/status"));
    let docs_href = format!("{}/api/docs", api::base_path());

    let info = move || match status.get() {
        None => view! { <Skeleton lines=6 /> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| status.refetch()) /> }.into_any()
        }
        Some(Ok(s)) => {
            let url_base = if s.url_base.is_empty() {
                "/".to_string()
            } else {
                s.url_base.clone()
            };
            let rows: Vec<(&str, String)> = vec![
                ("Version", s.version.clone()),
                ("Web UI", reelix_core::VERSION.to_string()),
                ("Platform", format!("{} / {}", s.os, s.arch)),
                ("Database", s.database.clone()),
                ("Data directory", s.data_dir.clone().unwrap_or_default()),
                ("URL base", url_base),
                ("Started", format::datetime(s.start_time)),
                ("Uptime", format::duration_secs(s.uptime_secs)),
            ];
            view! {
                <dl class="kv">
                    {rows
                        .into_iter()
                        .map(|(k, v)| view! { <div class="kv-row"><dt>{k}</dt><dd>{v}</dd></div> })
                        .collect_view()}
                </dl>
            }
            .into_any()
        }
    };

    view! {
        <PageHeader title="Status" subtitle="Version and runtime information.">
            <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| status.refetch()>
                <Icon name="refresh" />
                "Refresh"
            </button>
        </PageHeader>
        <div class="split-grid">
            <Card title="About" class="span-2">{info}</Card>
            <Card title="Connections">
                <dl class="kv">
                    <div class="kv-row">
                        <dt>"Live updates"</dt>
                        <dd>
                            {move || {
                                let s = live.status.get();
                                let tone = match s {
                                    WsStatus::Connected => Tone::Success,
                                    WsStatus::Idle => Tone::Neutral,
                                    _ => Tone::Warning,
                                };
                                view! { <StatusBadge tone=tone label=s.label() dot=true /> }
                            }}
                        </dd>
                    </div>
                </dl>
                <div class="link-list">
                    <a class="btn btn-secondary btn-sm" href=docs_href target="_blank" rel="noopener">
                        <Icon name="external" />
                        "API documentation"
                    </a>
                </div>
            </Card>
        </div>
    }
}
