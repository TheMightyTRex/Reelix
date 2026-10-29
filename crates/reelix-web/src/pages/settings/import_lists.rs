//! `/settings/importlists` — TMDB, Trakt, IMDb, MDBList, Plex watchlist and other list sources,
//! plus list sync status ("Sync now") and import list exclusions.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::integrations::{
    CreateExclusionRequest, ImportListExclusionDto, ImportListStatusDto, ImportListSyncRequest,
};
use reelix_api_types::providers::{ProviderDto, ProviderKind};
use reelix_core::MediaKind;

use crate::api;
use crate::components::library_common::on_entity_changed;
use crate::components::provider_form::ProviderListCard;
use crate::components::{
    Card, ConfirmDialog, DataTable, EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton,
};
use crate::format;
use crate::pages::system::require_admin;
use crate::state::Toasts;

#[component]
pub fn ImportListsPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Import Lists" subtitle="Lists Reelix syncs to add movies and series automatically." />
        <ProviderListCard kind=ProviderKind::ImportList title="Lists" />
        <SyncStatusCard />
        <ExclusionsCard />
    }
}

/// Last sync per list + "Sync now".
#[component]
fn SyncStatusCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let lists = LocalResource::new(|| api::get::<Vec<ProviderDto>>("/importlists"));
    let status = LocalResource::new(|| api::get::<Vec<ImportListStatusDto>>("/importlists/status"));
    on_entity_changed(&["import_list"], true, move || {
        lists.refetch();
        status.refetch();
    });
    let syncing = RwSignal::new(false);
    let sync = move |ids: Vec<i64>| {
        syncing.set(true);
        spawn_local(async move {
            match api::post::<_, EnqueuedJob>("/importlists/sync", &ImportListSyncRequest { list_ids: ids })
                .await
            {
                Ok(_) => toasts.success("Import list sync started."),
                Err(e) => toasts.error(format!("Could not start the sync: {e}")),
            }
            syncing.set(false);
        });
    };
    let body = move || {
        let (Some(lists), Some(status)) = (lists.get(), status.get()) else {
            return view! { <Skeleton lines=3 /> }.into_any();
        };
        let lists = match lists {
            Ok(l) => l,
            Err(e) => return view! { <ErrorBox error=e /> }.into_any(),
        };
        let status = status.unwrap_or_default();
        if lists.is_empty() {
            return view! {
                <EmptyState icon="list" title="No import lists" description="Add a list above to sync it here." compact=true />
            }
            .into_any();
        }
        view! {
            <DataTable caption="Import list sync status" headers=vec!["List", "Last sync", "Found", "Added", "Result", ""]>
                {lists
                    .into_iter()
                    .map(|l| {
                        let st = status.iter().find(|s| s.list_id == l.id).cloned();
                        let id = l.id;
                        view! {
                            <tr>
                                <td>
                                    {l.name.clone()}
                                    {(!l.enabled).then(|| view! { <span class="badge tone-neutral">"Disabled"</span> })}
                                </td>
                                <td>{st.as_ref().map_or_else(|| "Never".to_string(), |s| format::relative(s.last_sync))}</td>
                                <td>{st.as_ref().map(|s| s.items_found)}</td>
                                <td>{st.as_ref().map(|s| s.items_added)}</td>
                                <td>
                                    {match st.as_ref().map(|s| s.last_error.clone()) {
                                        Some(Some(e)) => view! { <span class="text-danger">{e}</span> }.into_any(),
                                        Some(None) => view! { <span class="text-success">"OK"</span> }.into_any(),
                                        None => view! { <span class="muted">"—"</span> }.into_any(),
                                    }}
                                </td>
                                <td class="cell-actions">
                                    <button type="button" class="btn btn-sm btn-ghost" disabled=move || syncing.get() || !l.enabled
                                        on:click=move |_| sync(vec![id])>
                                        <Icon name="refresh" />"Sync"
                                    </button>
                                </td>
                            </tr>
                        }
                    })
                    .collect_view()}
            </DataTable>
        }
        .into_any()
    };
    view! {
        <Card
            title="Sync"
            subtitle="Lists sync every 15 minutes once their own interval has passed."
            actions=view! {
                <button type="button" class="btn btn-sm btn-secondary" disabled=move || syncing.get() on:click=move |_| sync(Vec::new())>
                    <Icon name="refresh" />"Sync all now"
                </button>
            }
            .into_any()
        >
            {body}
        </Card>
    }
}

/// Media that import lists never add.
#[component]
fn ExclusionsCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let list = LocalResource::new(|| api::get::<Vec<ImportListExclusionDto>>("/importlists/exclusions"));
    on_entity_changed(&["import_list_exclusion"], false, move || list.refetch());

    let add_open = RwSignal::new(false);
    let kind = RwSignal::new(String::from("movie"));
    let tmdb = RwSignal::new(String::new());
    let title = RwSignal::new(String::new());
    let year = RwSignal::new(String::new());
    let clear_open = RwSignal::new(false);

    let open_add = move |_| {
        kind.set("movie".into());
        tmdb.set(String::new());
        title.set(String::new());
        year.set(String::new());
        add_open.set(true);
    };
    let submit = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let Ok(tmdb_id) = tmdb.get_untracked().trim().parse::<u32>() else {
            toasts.error("Enter a numeric TMDB id.");
            return;
        };
        let req = CreateExclusionRequest {
            kind: if kind.get_untracked() == "series" {
                MediaKind::Series
            } else {
                MediaKind::Movie
            },
            tmdb_id,
            title: title.get_untracked().trim().to_string(),
            year: year.get_untracked().trim().parse().ok(),
        };
        spawn_local(async move {
            match api::post::<_, ImportListExclusionDto>("/importlists/exclusions", &req).await {
                Ok(_) => {
                    add_open.set(false);
                    list.refetch();
                }
                Err(e) => toasts.error(format!("Could not add the exclusion: {e}")),
            }
        });
    };
    let remove = move |id: i64| {
        spawn_local(async move {
            match api::delete(&format!("/importlists/exclusions/{id}")).await {
                Ok(()) => list.refetch(),
                Err(e) => toasts.error(format!("Could not remove the exclusion: {e}")),
            }
        });
    };
    let clear_all = Callback::new(move |()| {
        spawn_local(async move {
            match api::delete("/importlists/exclusions").await {
                Ok(()) => list.refetch(),
                Err(e) => toasts.error(format!("Could not clear the exclusions: {e}")),
            }
        });
    });

    let body = move || {
        match list.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
        Some(Ok(v)) if v.is_empty() => view! {
            <EmptyState icon="shield" title="No exclusions"
                description="Excluded titles are skipped by every import list." compact=true />
        }
        .into_any(),
        Some(Ok(v)) => view! {
            <DataTable caption="Import list exclusions" headers=vec!["Title", "Type", "TMDB id", "Added", ""]>
                {v.into_iter()
                    .map(|x| {
                        let id = x.id;
                        let label = match x.year {
                            Some(y) => format!("{} ({y})", x.title),
                            None => x.title.clone(),
                        };
                        let icon = if x.kind == MediaKind::Movie { "film" } else { "tv" };
                        view! {
                            <tr>
                                <td>{label.clone()}</td>
                                <td><Icon name=icon />{if x.kind == MediaKind::Movie { " Movie" } else { " Series" }}</td>
                                <td class="mono">{x.tmdb_id}</td>
                                <td>{format::relative(x.created_at)}</td>
                                <td class="cell-actions">
                                    <button type="button" class="icon-btn icon-btn-sm" aria-label=format!("Remove exclusion for {label}")
                                        on:click=move |_| remove(id)>
                                        <Icon name="trash" />
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
        <Card
            title="Exclusions"
            subtitle="Titles import lists will never add."
            actions=view! {
                <div class="card-actions">
                    <button type="button" class="btn btn-sm btn-ghost"
                        disabled=move || list.get().and_then(Result::ok).is_none_or(|v| v.is_empty())
                        on:click=move |_| clear_open.set(true)>
                        "Clear all"
                    </button>
                    <button type="button" class="btn btn-sm btn-secondary" on:click=open_add>
                        <Icon name="plus" />"Add exclusion"
                    </button>
                </div>
            }
            .into_any()
        >
            {body}
        </Card>
        <Modal open=add_open title="Add exclusion">
            <form class="form" on:submit=submit>
                <div class="field-row">
                    <div class="field">
                        <label for="ex-kind">"Type"</label>
                        <select id="ex-kind" class="input" prop:value=move || kind.get() on:change=move |e| kind.set(event_target_value(&e))>
                            <option value="movie">"Movie"</option>
                            <option value="series">"Series"</option>
                        </select>
                    </div>
                    <div class="field">
                        <label for="ex-tmdb">"TMDB id"</label>
                        <input id="ex-tmdb" class="input mono" inputmode="numeric" required
                            prop:value=move || tmdb.get() on:input=move |e| tmdb.set(event_target_value(&e)) />
                    </div>
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="ex-title">"Title"</label>
                        <input id="ex-title" class="input" required
                            prop:value=move || title.get() on:input=move |e| title.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="ex-year">"Year " <span class="optional">"(optional)"</span></label>
                        <input id="ex-year" class="input" inputmode="numeric"
                            prop:value=move || year.get() on:input=move |e| year.set(event_target_value(&e)) />
                    </div>
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| add_open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary">"Add"</button>
                </div>
            </form>
        </Modal>
        <ConfirmDialog
            open=clear_open
            title="Clear all exclusions?"
            message=Signal::derive(|| "Import lists may add these titles again on their next sync.".to_string())
            confirm_label="Clear all"
            on_confirm=clear_all
        />
    }
}
