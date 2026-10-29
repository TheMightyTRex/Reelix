//! `/indexers/stats` — per-indexer counters with a simple bar visualisation.

use leptos::prelude::*;
use reelix_api_types::providers::IndexerStatsDto;

use crate::api;
use crate::components::library_common::on_entity_changed;
use crate::components::{Card, DataTable, EmptyState, ErrorBox, PageHeader, Skeleton, StatusBadge, Tone};
use crate::pages::system::require_admin;

fn bar(value: u64, max: u64) -> impl IntoView {
    let pct = if max == 0 {
        0.0
    } else {
        value as f64 * 100.0 / max as f64
    };
    view! {
        <div class="stat-bar" role="img" aria-label=format!("{value}")>
            <span class="stat-bar-fill" style=format!("width: {pct:.1}%")></span>
            <span class="stat-bar-label">{value.to_string()}</span>
        </div>
    }
}

#[component]
pub fn IndexerStatsPage() -> impl IntoView {
    require_admin();
    let stats = LocalResource::new(|| api::get::<Vec<IndexerStatsDto>>("/indexers/stats"));
    on_entity_changed(&["indexer"], false, move || stats.refetch());
    view! {
        <PageHeader title="Indexer stats" subtitle="Counters since the server started." />
        {move || match stats.get() {
            None => view! { <Skeleton lines=4 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| stats.refetch()) /> }.into_any(),
            Some(Ok(list)) if list.is_empty() => view! {
                <EmptyState icon="indexers" title="No indexers" description="Add an indexer to see statistics." />
            }.into_any(),
            Some(Ok(list)) => {
                let max_q = list.iter().map(|s| s.queries + s.rss_queries).max().unwrap_or(0);
                let max_g = list.iter().map(|s| s.grabs).max().unwrap_or(0);
                let max_ms = list.iter().map(|s| s.avg_response_ms).max().unwrap_or(0);
                view! {
                    <Card>
                        <DataTable caption="Indexer statistics" headers=vec!["Indexer", "Status", "Queries", "RSS", "Grabs", "Failures", "Avg response"]>
                            {list.into_iter().map(|s| {
                                let (tone, label) = if !s.enabled {
                                    (Tone::Neutral, "Disabled")
                                } else if s.status.disabled_until.is_some_and(|d| d > chrono::Utc::now()) {
                                    (Tone::Warning, "Backing off")
                                } else {
                                    (Tone::Success, "OK")
                                };
                                view! {
                                    <tr>
                                        <td>{s.name.clone()}</td>
                                        <td><StatusBadge tone=tone label=label dot=true /></td>
                                        <td>{bar(s.queries + s.rss_queries, max_q)}</td>
                                        <td>{s.rss_queries.to_string()}</td>
                                        <td>{bar(s.grabs, max_g)}</td>
                                        <td>{s.failures.to_string()}</td>
                                        <td>{bar(s.avg_response_ms, max_ms)}<span class="field-hint">" ms"</span></td>
                                    </tr>
                                }
                            }).collect_view()}
                        </DataTable>
                    </Card>
                }.into_any()
            }
        }}
    }
}
