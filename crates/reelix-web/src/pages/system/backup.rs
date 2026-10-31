//! `/system/backup` — scheduled and manual backups, downloads and restore.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::system::{BackupDto, BackupKind, BackupSettings, BackupsOverview, PendingRestoreDto};
use wasm_bindgen::JsCast;

use super::require_admin;
use crate::api;
use crate::components::library_common::bytes;
use crate::components::{
    Card, ConfirmDialog, DataTable, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge, Tone,
};
use crate::format;
use crate::state::Toasts;

/// What a confirm dialog is about to do.
#[derive(Clone)]
enum Pending {
    Restore(String),
    Delete(String),
    Upload(web_sys::File),
}

#[component]
pub fn BackupPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let data = LocalResource::new(|| api::get::<BackupsOverview>("/system/backups"));
    let busy = RwSignal::new(false);
    let restore_open = RwSignal::new(false);
    let delete_open = RwSignal::new(false);
    let pending = RwSignal::new(None::<Pending>);

    let back_up_now = move |_| {
        busy.set(true);
        spawn_local(async move {
            match api::post_empty::<BackupDto>("/system/backups").await {
                Ok(b) => toasts.success(format!("Backup {} created.", b.name)),
                Err(e) => toasts.error(format!("Backup failed: {e}")),
            }
            busy.set(false);
            data.refetch();
        });
    };

    let ask = move |p: Pending| {
        let open = if matches!(p, Pending::Delete(_)) {
            delete_open
        } else {
            restore_open
        };
        pending.set(Some(p));
        open.set(true);
    };

    let staged = move |r: api::ApiResult<PendingRestoreDto>| match r {
        Ok(_) => toasts.success("Restore staged. Restart Reelix to finish restoring."),
        Err(e) => toasts.error(format!("Could not restore: {e}")),
    };

    let confirm = Callback::new(move |()| {
        let Some(p) = pending.get_untracked() else { return };
        busy.set(true);
        spawn_local(async move {
            match p {
                Pending::Restore(name) => staged(
                    api::post_empty::<PendingRestoreDto>(&format!("/system/backups/{name}/restore")).await,
                ),
                Pending::Upload(file) => {
                    staged(api::post_file::<PendingRestoreDto>("/system/backups/restore", &file).await)
                }
                Pending::Delete(name) => match api::delete(&format!("/system/backups/{name}")).await {
                    Ok(()) => toasts.success(format!("Deleted {name}.")),
                    Err(e) => toasts.error(format!("Could not delete {name}: {e}")),
                },
            }
            busy.set(false);
            data.refetch();
        });
    });

    let confirm_message = Signal::derive(move || match pending.get() {
        Some(Pending::Restore(name)) => format!(
            "Reelix will replace its database, config.toml and secret key with the ones in {name} the next time it starts. \
             Everything changed since that backup will be lost. The current files are kept in the restore folder."
        ),
        Some(Pending::Upload(file)) => format!(
            "Reelix will replace its database, config.toml and secret key with the ones in {} the next time it starts. \
             Everything changed since that backup will be lost. The current files are kept in the restore folder.",
            file.name()
        ),
        Some(Pending::Delete(name)) => format!("{name} will be deleted permanently."),
        None => String::new(),
    });
    let cancel_restore = move |_| {
        spawn_local(async move {
            match api::delete("/system/backups/restore").await {
                Ok(()) => toasts.info("Restore cancelled."),
                Err(e) => toasts.error(format!("Could not cancel the restore: {e}")),
            }
            data.refetch();
        });
    };

    let on_file = move |e: leptos::ev::Event| {
        let input = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok());
        if let Some(input) = input {
            if let Some(file) = input.files().and_then(|f| f.get(0)) {
                ask(Pending::Upload(file));
            }
            // Allow picking the same file again.
            input.set_value("");
        }
    };

    let content = move || match data.get() {
        None => view! { <Card><Skeleton lines=6 /></Card> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| data.refetch()) /> }.into_any()
        }
        Some(Ok(o)) => {
            let restore_banner = o.pending_restore.clone().map(|p| view! {
                <div class="alert alert-warning" role="status">
                    <Icon name="warning" />
                    <div class="alert-body">
                        <strong>"Restart Reelix to finish restoring"</strong>
                        <span>{format!(
                            "A restore from {} was staged {}. It replaces the current data when Reelix next starts.",
                            p.source,
                            format::relative(p.staged_at)
                        )}</span>
                    </div>
                    <button type="button" class="btn btn-sm btn-ghost" on:click=cancel_restore>"Cancel restore"</button>
                </div>
            });
            let key_note = (!o.includes_secret_key).then(|| view! {
                <div class="alert alert-info" role="note">
                    <Icon name="info" />
                    <div class="alert-body">
                        <strong>"The secret key isn't backed up"</strong>
                        <span>"It comes from REELIX_SECRET_KEY. Keep that value safe: without it, saved passwords and API keys in a restored backup can't be decrypted."</span>
                    </div>
                </div>
            });
            view! {
                {restore_banner}
                {key_note}
                <BackupSchedule settings=o.settings.clone() />
                <Card title="Backups" subtitle=format!("Stored in {}. Backups contain the secret key that protects saved passwords, so keep downloads safe.", o.backup_dir)>
                    <BackupTable backups=o.backups.clone() on_ask=Callback::new(ask) />
                </Card>
            }
            .into_any()
        }
    };

    view! {
        <PageHeader title="Backup" subtitle="Back up and restore the database, config.toml and secret key.">
            <label class="btn btn-secondary btn-sm file-button">
                <Icon name="folder" />
                "Restore from file…"
                <input type="file" accept=".zip,application/zip" class="visually-hidden" on:change=on_file
                    disabled=move || busy.get() />
            </label>
            <button type="button" class="btn btn-primary btn-sm" disabled=move || busy.get() on:click=back_up_now>
                <Icon name="download" />
                {move || if busy.get() { "Working…" } else { "Back up now" }}
            </button>
        </PageHeader>
        <div class="stack">{content}</div>
        <ConfirmDialog open=restore_open title="Restore this backup?" message=confirm_message
            confirm_label="Stage restore" on_confirm=confirm />
        <ConfirmDialog open=delete_open title="Delete backup?" message=confirm_message
            confirm_label="Delete" on_confirm=confirm />
    }
}

#[component]
fn BackupTable(backups: Vec<BackupDto>, on_ask: Callback<Pending>) -> impl IntoView {
    if backups.is_empty() {
        return view! {
            <EmptyState icon="folder" title="No backups yet" compact=true
                description="Use “Back up now”, or turn on scheduled backups above." />
        }
        .into_any();
    }
    view! {
        <DataTable caption="Backups" headers=vec!["Backup", "Type", "Created", "Size", ""]>
            {backups
                .into_iter()
                .map(|b| {
                    let href = api::api_url(&format!("/system/backups/{}", b.name));
                    let restore_name = b.name.clone();
                    let delete_name = b.name.clone();
                    let (tone, label) = match b.kind {
                        BackupKind::Manual => (Tone::Info, "Manual"),
                        BackupKind::Scheduled => (Tone::Neutral, "Scheduled"),
                    };
                    view! {
                        <tr>
                            <td>
                                <span class="cell-strong mono">{b.name.clone()}</span>
                                {b.version.clone().map(|v| view! { <span class="cell-sub">{format!("Reelix {v}")}</span> })}
                            </td>
                            <td><StatusBadge tone=tone label=label /></td>
                            <td title=format::datetime(b.created_at)>{format::relative(b.created_at)}</td>
                            <td>{bytes(b.size_bytes)}</td>
                            <td class="cell-actions">
                                <a class="btn btn-sm btn-secondary" href=href download=b.name.clone()
                                    aria-label=format!("Download {}", b.name)>
                                    <Icon name="download" />
                                    "Download"
                                </a>
                                <button type="button" class="btn btn-sm btn-secondary"
                                    aria-label=format!("Restore {}", b.name)
                                    on:click=move |_| on_ask.run(Pending::Restore(restore_name.clone()))>
                                    <Icon name="refresh" />
                                    "Restore"
                                </button>
                                <button type="button" class="btn btn-sm btn-ghost"
                                    aria-label=format!("Delete {}", b.name)
                                    on:click=move |_| on_ask.run(Pending::Delete(delete_name.clone()))>
                                    <Icon name="trash" />
                                </button>
                            </td>
                        </tr>
                    }
                })
                .collect_view()}
        </DataTable>
    }
    .into_any()
}

#[component]
fn BackupSchedule(settings: BackupSettings) -> impl IntoView {
    let toasts = Toasts::expect();
    let enabled = RwSignal::new(settings.interval_days > 0);
    let interval = RwSignal::new(
        if settings.interval_days > 0 {
            settings.interval_days
        } else {
            7
        }
        .to_string(),
    );
    let retention = RwSignal::new(settings.retention.to_string());
    let saving = RwSignal::new(false);

    let save = move |_| {
        let days = if enabled.get_untracked() {
            match interval.get_untracked().trim().parse::<u32>() {
                Ok(d) if (1..=365).contains(&d) => d,
                _ => {
                    toasts.error("Back up every 1 to 365 days.");
                    return;
                }
            }
        } else {
            0
        };
        let Ok(keep) = retention.get_untracked().trim().parse::<u32>() else {
            toasts.error("Keep between 1 and 100 scheduled backups.");
            return;
        };
        let body = BackupSettings {
            interval_days: days,
            retention: keep,
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, BackupSettings>("/system/backups/settings", &body).await {
                Ok(_) => toasts.success("Backup schedule saved."),
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    view! {
        <Card title="Schedule" subtitle="The Scheduled backup task checks once a day. Manual backups are never deleted automatically.">
            <div class="form">
                <label class="checkbox toggle-row">
                    <input type="checkbox" prop:checked=move || enabled.get()
                        on:change=move |e| enabled.set(event_target_checked(&e)) />
                    <span>
                        <span class="perm-label">"Scheduled backups"</span>
                        <span class="perm-desc">"Back up automatically and keep the most recent ones."</span>
                    </span>
                </label>
                <div class="field-row">
                    <div class="field">
                        <label for="bk-interval">"Back up every (days)"</label>
                        <input id="bk-interval" class="input" type="number" min="1" max="365" inputmode="numeric"
                            disabled=move || !enabled.get()
                            prop:value=move || interval.get() on:input=move |e| interval.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="bk-keep">"Scheduled backups to keep"</label>
                        <input id="bk-keep" class="input" type="number" min="1" max="100" inputmode="numeric"
                            prop:value=move || retention.get() on:input=move |e| retention.set(event_target_value(&e)) />
                    </div>
                </div>
                <div class="form-actions">
                    <button type="button" class="btn btn-primary" on:click=save disabled=move || saving.get()>
                        {move || if saving.get() { "Saving…" } else { "Save schedule" }}
                    </button>
                </div>
            </div>
        </Card>
    }
}
