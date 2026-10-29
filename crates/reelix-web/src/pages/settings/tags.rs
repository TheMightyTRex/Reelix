//! `/settings/tags` — tags linking media to indexers, clients, notifications and profiles.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::providers::{TagDto, TagInput};

use crate::api;
use crate::components::library_common::on_entity_changed;
use crate::components::{Card, ConfirmDialog, EmptyState, ErrorBox, Icon, PageHeader, Skeleton};
use crate::pages::system::require_admin;
use crate::state::Toasts;

#[component]
pub fn TagsPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let tags = LocalResource::new(|| api::get::<Vec<TagDto>>("/tags"));
    on_entity_changed(&["tag"], false, move || tags.refetch());
    let label = RwSignal::new(String::new());
    let delete_open = RwSignal::new(false);
    let target = RwSignal::new(None::<TagDto>);

    let add = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let body = TagInput {
            label: label.get_untracked().trim().to_owned(),
        };
        if body.label.is_empty() {
            return;
        }
        spawn_local(async move {
            match api::post::<_, TagDto>("/tags", &body).await {
                Ok(t) => {
                    toasts.success(format!("Added tag “{}”.", t.label));
                    label.set(String::new());
                    tags.refetch();
                }
                Err(e) => toasts.error(format!("Could not add tag: {e}")),
            }
        });
    };
    let confirm = Callback::new(move |()| {
        let Some(t) = target.get_untracked() else { return };
        spawn_local(async move {
            match api::delete(&format!("/tags/{}", t.id)).await {
                Ok(()) => {
                    toasts.success(format!("Deleted tag “{}”.", t.label));
                    tags.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    view! {
        <PageHeader title="Tags" subtitle="Tags restrict indexers, download clients and notifications to matching media." />
        <Card title="Tags">
            <form class="toolbar" on:submit=add>
                <input class="input" type="text" maxlength="64" placeholder="New tag" aria-label="New tag label"
                    prop:value=move || label.get() on:input=move |e| label.set(event_target_value(&e)) />
                <button type="submit" class="btn btn-primary btn-sm"><Icon name="plus" />"Add"</button>
            </form>
            {move || match tags.get() {
                None => view! { <Skeleton lines=2 /> }.into_any(),
                Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
                Some(Ok(list)) if list.is_empty() => view! { <EmptyState icon="list" title="No tags yet" compact=true /> }.into_any(),
                Some(Ok(list)) => view! {
                    <div class="chip-list">
                        {list.into_iter().map(|t| {
                            let tt = t.clone();
                            view! {
                                <span class="chip tag-chip">
                                    {t.label.clone()}
                                    <button type="button" class="icon-btn icon-btn-sm" aria-label=format!("Delete tag {}", t.label)
                                        on:click=move |_| { target.set(Some(tt.clone())); delete_open.set(true); }>"×"</button>
                                </span>
                            }
                        }).collect_view()}
                    </div>
                }.into_any(),
            }}
        </Card>
        <ConfirmDialog open=delete_open title="Delete tag?"
            message=Signal::derive(move || target.get().map(|t| format!("Delete “{}”? It is removed from every item using it.", t.label)).unwrap_or_default())
            confirm_label="Delete" on_confirm=confirm />
    }
}
