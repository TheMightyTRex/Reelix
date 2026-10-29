//! List / create / revoke the signed-in user's API keys.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::{API_KEY_HEADER, ApiKeyDto, CreateApiKeyRequest, CreatedApiKey};

use super::{Card, ConfirmDialog, CopyButton, DataTable, EmptyState, ErrorBox, Icon, Modal, Skeleton};
use crate::api;
use crate::format;
use crate::state::Toasts;

#[component]
pub fn ApiKeysPanel() -> impl IntoView {
    let toasts = Toasts::expect();
    let keys = LocalResource::new(|| api::get::<Vec<ApiKeyDto>>("/auth/apikeys"));

    let create_open = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let creating = RwSignal::new(false);
    let created = RwSignal::new(None::<CreatedApiKey>);
    let revoke_target = RwSignal::new(None::<ApiKeyDto>);
    let revoke_open = RwSignal::new(false);

    let open_create = move |_| {
        name.set(String::new());
        created.set(None);
        create_open.set(true);
    };

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let key_name = name.get_untracked().trim().to_string();
        if key_name.is_empty() || creating.get_untracked() {
            return;
        }
        creating.set(true);
        spawn_local(async move {
            match api::post::<_, CreatedApiKey>("/auth/apikeys", &CreateApiKeyRequest { name: key_name })
                .await
            {
                Ok(key) => {
                    created.set(Some(key));
                    keys.refetch();
                }
                Err(e) => toasts.error(format!("Could not create API key: {e}")),
            }
            creating.set(false);
        });
    };

    let confirm_revoke = Callback::new(move |()| {
        let Some(key) = revoke_target.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/auth/apikeys/{}", key.id)).await {
                Ok(()) => {
                    toasts.success(format!("Revoked API key “{}”.", key.name));
                    keys.refetch();
                }
                Err(e) => toasts.error(format!("Could not revoke key: {e}")),
            }
        });
    });

    let list = move || {
        match keys.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| keys.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState
                icon="key"
                title="No API keys yet"
                description="Create a key to let scripts and companion apps talk to Reelix on your behalf."
                compact=true
            />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <DataTable caption="API keys" headers=vec!["Name", "Key", "Created", "Last used", ""]>
                {list
                    .into_iter()
                    .map(|key| {
                        let target = key.clone();
                        view! {
                            <tr>
                                <td class="cell-strong">{key.name.clone()}</td>
                                <td>
                                    <code class="mono">"rlx_" {key.prefix.clone()} "…"</code>
                                </td>
                                <td title=format::datetime(key.created_at)>{format::relative(key.created_at)}</td>
                                <td>
                                    {key
                                        .last_used_at
                                        .map(format::relative)
                                        .unwrap_or_else(|| "Never".into())}
                                </td>
                                <td class="cell-actions">
                                    <button
                                        type="button"
                                        class="btn btn-sm btn-ghost btn-danger-text"
                                        aria-label=format!("Revoke API key {}", key.name)
                                        on:click=move |_| {
                                            revoke_target.set(Some(target.clone()));
                                            revoke_open.set(true);
                                        }
                                    >
                                        <Icon name="trash" />
                                        "Revoke"
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

    let created_key =
        Signal::derive(move || created.with(|c| c.as_ref().map(|c| c.key.clone()).unwrap_or_default()));

    view! {
        <Card
            title="API keys"
            subtitle=format!("Send a key in the {API_KEY_HEADER} header (or ?apikey=) to use the API without a session.")
            actions=view! {
                <button type="button" class="btn btn-primary btn-sm" on:click=open_create>
                    <Icon name="plus" />
                    "New key"
                </button>
            }
                .into_any()
        >
            {list}
        </Card>

        <Modal open=create_open title="Create API key">
            <Show
                when=move || created.with(Option::is_some)
                fallback=move || {
                    view! {
                        <form class="form" on:submit=submit>
                            <div class="field">
                                <label for="apikey-name">"Name"</label>
                                <input
                                    id="apikey-name"
                                    class="input"
                                    type="text"
                                    maxlength="64"
                                    required
                                    autofocus
                                    placeholder="e.g. Home dashboard"
                                    prop:value=move || name.get()
                                    on:input=move |e| name.set(event_target_value(&e))
                                />
                                <p class="field-hint">"Helps you recognise the key later."</p>
                            </div>
                            <div class="modal-footer">
                                <button type="button" class="btn btn-ghost" on:click=move |_| create_open.set(false)>
                                    "Cancel"
                                </button>
                                <button
                                    type="submit"
                                    class="btn btn-primary"
                                    disabled=move || creating.get() || name.with(|n| n.trim().is_empty())
                                >
                                    {move || if creating.get() { "Creating…" } else { "Create key" }}
                                </button>
                            </div>
                        </form>
                    }
                }
            >
                <div class="alert alert-warning">
                    <Icon name="warning" />
                    <div class="alert-body">
                        <strong>"Copy this key now"</strong>
                        <span>"For your security it is shown only once and can't be retrieved later."</span>
                    </div>
                </div>
                <div class="key-reveal">
                    <code class="mono key-value">{move || created_key.get()}</code>
                    <CopyButton text=created_key />
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-primary" on:click=move |_| create_open.set(false)>
                        "Done"
                    </button>
                </div>
            </Show>
        </Modal>

        <ConfirmDialog
            open=revoke_open
            title="Revoke API key?"
            message=Signal::derive(move || {
                revoke_target
                    .with(|k| {
                        k.as_ref()
                            .map(|k| {
                                format!(
                                    "Anything using “{}” will immediately lose access. This can't be undone.",
                                    k.name,
                                )
                            })
                            .unwrap_or_default()
                    })
            })
            confirm_label="Revoke key"
            on_confirm=confirm_revoke
        />
    }
}
