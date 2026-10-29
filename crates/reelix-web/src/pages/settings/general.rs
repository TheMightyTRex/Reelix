//! `/settings/general` — API keys plus (read-only for now) server settings.

use leptos::prelude::*;
use reelix_api_types::SystemStatus;

use crate::api;
use crate::components::{ApiKeysPanel, Card, PageHeader, Skeleton};
use crate::pages::system::require_admin;

#[component]
pub fn GeneralPage() -> impl IntoView {
    require_admin();
    let status = LocalResource::new(|| api::get::<SystemStatus>("/system/status"));

    let server = move || match status.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => view! { <p class="muted">{e.message}</p> }.into_any(),
        Some(Ok(s)) => {
            let url_base = if s.url_base.is_empty() {
                "/ (none)".to_string()
            } else {
                s.url_base
            };
            view! {
                <dl class="kv">
                    <div class="kv-row"><dt>"URL base"</dt><dd class="mono">{url_base}</dd></div>
                    <div class="kv-row"><dt>"Data directory"</dt><dd class="mono">{s.data_dir}</dd></div>
                    <div class="kv-row"><dt>"Database"</dt><dd>{s.database}</dd></div>
                </dl>
            }
            .into_any()
        }
    };

    view! {
        <PageHeader title="General" subtitle="Security and server settings." />
        <div class="stack">
            <ApiKeysPanel />
            <Card
                title="Server"
                subtitle="Bind address, port, URL base and logging are set in config.toml or REELIX_* environment variables. Editing them here arrives in M8."
            >
                {server}
            </Card>
        </div>
    }
}
