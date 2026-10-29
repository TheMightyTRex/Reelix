//! `/system/health` — health checks.

use leptos::prelude::*;
use reelix_api_types::{HealthLevel, HealthReport};

use super::require_admin;
use crate::api;
use crate::components::{
    Card, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge, Tone, health_label,
};

#[component]
pub fn HealthPage() -> impl IntoView {
    require_admin();
    let report = LocalResource::new(|| api::get::<HealthReport>("/system/health"));

    let body = move || match report.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| report.refetch()) /> }.into_any()
        }
        Some(Ok(r)) => {
            let summary = match r.status {
                HealthLevel::Ok => "Everything is working as expected.",
                HealthLevel::Warning => "Some checks need your attention.",
                HealthLevel::Error => "Reelix found problems that may stop it from working.",
            };
            let mut checks = r.checks;
            checks.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.source.cmp(&b.source)));
            view! {
                <div class=format!("health-summary {}", Tone::health(r.status).class())>
                    <Icon name=match r.status {
                        HealthLevel::Ok => "check",
                        HealthLevel::Warning => "warning",
                        HealthLevel::Error => "error",
                    } />
                    <div>
                        <strong>{health_label(r.status)}</strong>
                        <p>{summary}</p>
                    </div>
                </div>
                {if checks.is_empty() {
                    view! { <EmptyState icon="heart" title="No checks reported" compact=true /> }.into_any()
                } else {
                    view! {
                        <ul class="check-list">
                            {checks
                                .into_iter()
                                .map(|c| {
                                    view! {
                                        <li class="health-check">
                                            <StatusBadge tone=Tone::health(c.level) label=health_label(c.level) dot=true />
                                            <span class="health-source">{c.source}</span>
                                            <span class="health-message">{c.message}</span>
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                    .into_any()
                }}
            }
            .into_any()
        }
    };

    view! {
        <PageHeader title="Health" subtitle="Checks Reelix runs against its own components.">
            <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| report.refetch()>
                <Icon name="refresh" />
                "Re-run checks"
            </button>
        </PageHeader>
        <Card>{body}</Card>
    }
}
