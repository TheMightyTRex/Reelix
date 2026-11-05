//! `/activity/blocklist` — releases that failed or were rejected; remove entries or clear all.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::pipeline::{BlocklistDto, BulkIdsRequest, Paged};

use crate::api;
use crate::components::history_table::{MediaCell, ProtoBadge, quality_label};
use crate::components::infinite::{InfiniteSentinel, PagedList};
use crate::components::library_common::{on_entity_changed, toggle_id};
use crate::components::library_forms::{delete_json, require_library, with_query};
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, PageHeader, Skeleton};
use crate::format;
use crate::state::Toasts;

const PAGE_SIZE: u32 = 50;

#[component]
pub fn BlocklistPage() -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let selected = RwSignal::new(Vec::<i64>::new());
    let clear_open = RwSignal::new(false);
    let list = PagedList::new(
        PAGE_SIZE,
        |b: &BlocklistDto| b.id,
        |page, size| {
            let url = with_query(
                "/blocklist",
                &[("page", page.to_string()), ("page_size", size.to_string())],
            );
            async move {
                api::get::<Paged<BlocklistDto>>(&url)
                    .await
                    .map(|p| (p.items, p.total))
            }
        },
    );
    list.reset();
    on_entity_changed(&["blocklist"], false, move || list.refresh());
    let total = list.total;

    let remove = move |ids: Vec<i64>, what: String| {
        spawn_local(async move {
            let res = if let [one] = ids.as_slice() {
                api::delete(&format!("/blocklist/{one}")).await
            } else {
                delete_json("/blocklist", &BulkIdsRequest { ids }).await
            };
            match res {
                Ok(()) => {
                    toasts.success(what);
                    selected.set(Vec::new());
                    list.refresh();
                }
                Err(e) => toasts.error(format!("Could not update the blocklist: {e}")),
            }
        });
    };
    let clear_all = Callback::new(move |()| remove(Vec::new(), "Blocklist cleared.".into()));

    let body = move || {
        if !list.ready.get() {
            return view! { <Skeleton lines=5 /> }.into_any();
        }
        let items = list.items.get();
        if items.is_empty() {
            return match list.error.get() {
                Some(e) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| list.reset()) /> }.into_any(),
                None => view! {
                    <EmptyState icon="shield" title="The blocklist is empty"
                        description="Releases that fail to download or import are added here so they aren't grabbed again." />
                }
                .into_any(),
            };
        }
        {
            let ids: Vec<i64> = items.iter().map(|b| b.id).collect();
            let ids2 = ids.clone();
            // Select-all covers the rows loaded so far.
            let all_label = format!("Select all {} loaded entries", ids.len());
            view! {
                <div class="table-wrap">
                    <table class="table">
                        <caption class="visually-hidden">"Blocklist"</caption>
                        <thead>
                            <tr>
                                <th scope="col" class="col-check">
                                    <input type="checkbox" aria-label=all_label.clone() title=all_label
                                        prop:checked=move || !ids.is_empty() && selected.with(|s| ids.iter().all(|i| s.contains(i)))
                                        on:change=move |e| {
                                            if event_target_checked(&e) { selected.set(ids2.clone()) } else { selected.set(Vec::new()) }
                                        } />
                                </th>
                                <th scope="col">"Media"</th>
                                <th scope="col">"Release"</th>
                                <th scope="col">"Quality"</th>
                                <th scope="col">"Indexer"</th>
                                <th scope="col">"Reason"</th>
                                <th scope="col">"Date"</th>
                                <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                            </tr>
                        </thead>
                        <tbody>
                            {items.into_iter().map(|b| {
                                let id = b.id;
                                let select_label = format!("Select {}", b.source_title);
                                view! {
                                    <tr class:is-selected=move || selected.with(|s| s.contains(&id))>
                                        <td class="col-check">
                                            <input type="checkbox" aria-label=select_label.clone()
                                                prop:checked=move || selected.with(|s| s.contains(&id))
                                                on:change=move |_| toggle_id(selected, id) />
                                        </td>
                                        <td><MediaCell media=b.media.clone() /></td>
                                        <td class="release-title">
                                            {b.protocol.map(|p| view! { <ProtoBadge protocol=p /> })}
                                            " "
                                            <span class="mono">{b.source_title.clone()}</span>
                                        </td>
                                        <td>{b.quality.as_ref().map(quality_label).unwrap_or_else(|| "—".into())}</td>
                                        <td>{b.indexer.clone().unwrap_or_else(|| "—".into())}</td>
                                        <td class="bl-reason">{b.message.clone().unwrap_or_else(|| "—".into())}</td>
                                        <td title=format::datetime(b.date)>{format::relative(b.date)}</td>
                                        <td class="cell-actions">
                                            <button type="button" class="btn btn-sm btn-ghost btn-danger-text" title="Remove from blocklist"
                                                on:click=move |_| remove(vec![id], "Removed from the blocklist.".into())>
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
                {move || list.error.get().map(|e| view! { <ErrorBox error=e on_retry=Callback::new(move |()| list.retry()) /> })}
            }
            .into_any()
        }
    };

    view! {
        <PageHeader title="Blocklist" subtitle="Releases Reelix won't grab again, and why.">
            <Show when=move || selected.with(|s| !s.is_empty())>
                <button type="button" class="btn btn-secondary"
                    on:click=move |_| {
                        let ids = selected.get_untracked();
                        let n = ids.len();
                        remove(ids, format!("Removed {n} entr{}.", if n == 1 { "y" } else { "ies" }));
                    }>
                    <Icon name="trash" />
                    {move || format!("Remove selected ({})", selected.with(Vec::len))}
                </button>
            </Show>
            <button type="button" class="btn btn-ghost btn-danger-text" disabled=move || total.get() == 0
                on:click=move |_| clear_open.set(true)>
                <Icon name="trash" />
                "Clear all"
            </button>
        </PageHeader>
        <div class="card">
            <div class="card-body">
                {body}
                <InfiniteSentinel
                    on_visible=Callback::new(move |()| list.load_more())
                    enabled=Signal::derive(move || list.can_load_more())
                    loading=list.loading
                    loaded=Signal::derive(move || list.loaded())
                    total=list.total
                />
            </div>
        </div>
        <ConfirmDialog open=clear_open title="Clear the blocklist?"
            message=Signal::derive(move || format!("All {} entries are removed. Those releases can be grabbed again.", total.get()))
            confirm_label="Clear blocklist" on_confirm=clear_all />
    }
}
