//! `/system/updates` — installed version, new releases from GitHub, their release notes, and
//! installing an update in place.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::system::{
    InstallKind, InstallUpdateRequest, ReleaseDto, SelfUpdateDto, SelfUpdateState, UpdateSettings,
    UpdateStatusDto,
};
use serde::Deserialize;

use super::require_admin;
use crate::api;
use crate::components::library_common::bytes;
use crate::components::{
    Card, ConfirmDialog, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, Spinner, StatusBadge, Tone,
};
use crate::format;
use crate::state::Toasts;

const POLL: Duration = Duration::from_secs(2);
/// How long to wait for the restarted server before giving up.
const RESTART_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The part of the status read while the server restarts: the new version's status may have a
/// different shape.
#[derive(Deserialize)]
struct RunningVersion {
    current_version: String,
}

/// Release asset name fragment for the platform of this install.
fn asset_hint(install: InstallKind) -> Option<&'static str> {
    match install {
        InstallKind::Windows => Some("windows"),
        _ => None,
    }
}

fn state_text(s: &SelfUpdateDto) -> String {
    let version = s.version.clone().unwrap_or_default();
    match s.state {
        SelfUpdateState::Downloading => format!("Downloading Reelix {version}…"),
        SelfUpdateState::Verifying => "Verifying the download…".into(),
        SelfUpdateState::BackingUp => "Backing up the database…".into(),
        SelfUpdateState::Installing => format!("Installing Reelix {version}…"),
        SelfUpdateState::Restarting => "Restarting Reelix… This page reloads when it's back.".into(),
        SelfUpdateState::Idle | SelfUpdateState::Failed => String::new(),
    }
}

#[component]
pub fn UpdatesPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let status = LocalResource::new(|| api::get::<UpdateStatusDto>("/system/updates"));
    // Set after "Check now" (and while installing) so the page shows the fresh result.
    let fresh = RwSignal::new(None::<UpdateStatusDto>);
    let checking = RwSignal::new(false);
    let polling = RwSignal::new(false);
    let confirm_open = RwSignal::new(false);
    let confirm_version = RwSignal::new(String::new());

    let current = move || fresh.get().map(Ok).or_else(|| status.get());

    // Follows an install until it fails or the restarted server reports a new version.
    let follow_install = move |from_version: String| {
        if polling.get_untracked() {
            return;
        }
        polling.set(true);
        spawn_local(async move {
            let mut restarting_since = None::<chrono::DateTime<chrono::Utc>>;
            loop {
                gloo_timers::future::sleep(POLL).await;
                if let Some(since) = restarting_since {
                    // The server is down or the old one is still shutting down: keep asking.
                    if let Ok(r) = api::get::<RunningVersion>("/system/updates").await
                        && r.current_version != from_version
                    {
                        toasts.success(format!("Reelix {} is installed.", r.current_version));
                        if let Some(w) = web_sys::window() {
                            let _ = w.location().reload();
                        }
                        break;
                    }
                    if (chrono::Utc::now() - since).to_std().unwrap_or_default() > RESTART_TIMEOUT {
                        toasts.error("Reelix hasn't come back after the update. Check that it's running and look at its logs.");
                        break;
                    }
                    continue;
                }
                match api::get::<UpdateStatusDto>("/system/updates").await {
                    Ok(s) => {
                        let state = s.self_update.state;
                        let error = s.self_update.error.clone();
                        fresh.set(Some(s));
                        match state {
                            SelfUpdateState::Restarting => restarting_since = Some(chrono::Utc::now()),
                            SelfUpdateState::Failed => {
                                toasts.error(format!(
                                    "The update failed: {}",
                                    error.unwrap_or_else(|| "unknown error".into())
                                ));
                                break;
                            }
                            SelfUpdateState::Idle => break,
                            _ => {}
                        }
                    }
                    // The server stopped answering mid-install: it is most likely restarting.
                    Err(_) => restarting_since = Some(chrono::Utc::now()),
                }
            }
            polling.set(false);
        });
    };

    // Also follow an install started elsewhere (another tab, or the automatic install).
    Effect::new(move || {
        if let Some(Ok(s)) = current()
            && s.self_update.state.busy()
        {
            follow_install(s.current_version.clone());
        }
    });

    let check_now = move |_| {
        checking.set(true);
        spawn_local(async move {
            match api::post_empty::<UpdateStatusDto>("/system/updates/check").await {
                Ok(s) => {
                    match (&s.error, s.update_available) {
                        (Some(e), _) => toasts.error(format!("Update check failed: {e}")),
                        (None, true) => toasts.info("An update is available."),
                        (None, false) => toasts.success("Reelix is up to date."),
                    }
                    fresh.set(Some(s));
                }
                Err(e) => toasts.error(format!("Update check failed: {e}")),
            }
            checking.set(false);
        });
    };

    let save_settings = move |settings: UpdateSettings| {
        spawn_local(async move {
            match api::put::<_, UpdateStatusDto>("/system/updates/settings", &settings).await {
                Ok(s) => {
                    toasts.success(match (settings.automatic, settings.auto_install) {
                        (false, _) => "Automatic update checks are off.",
                        (true, false) => "Reelix will check for updates twice a day.",
                        (true, true) => "Reelix will check for updates twice a day and install them.",
                    });
                    fresh.set(Some(s));
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };

    let ask_install = move |version: String| {
        confirm_version.set(version);
        confirm_open.set(true);
    };
    let install = move |()| {
        let version = confirm_version.get_untracked();
        spawn_local(async move {
            match api::post::<_, UpdateStatusDto>(
                "/system/updates/install",
                &InstallUpdateRequest { version },
            )
            .await
            {
                Ok(s) => {
                    let from = s.current_version.clone();
                    fresh.set(Some(s));
                    follow_install(from);
                }
                Err(e) => toasts.error(format!("Could not start the update: {e}")),
            }
        });
    };
    let confirm_message = Signal::derive(move || {
        format!(
            "Reelix downloads {} from GitHub, verifies it, makes a backup of the database first, \
             replaces its program files and restarts. It is unavailable for a moment while it restarts.",
            confirm_version.get()
        )
    });

    let content = move || match current() {
        None => view! { <Card><Skeleton lines=6 /></Card> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| status.refetch()) /> }.into_any()
        }
        Some(Ok(s)) => view! {
            <UpdatesView s=s on_settings=Callback::new(save_settings) on_install=Callback::new(ask_install) />
        }
        .into_any(),
    };

    view! {
        <PageHeader title="Updates" subtitle="New Reelix releases and what changed.">
            <button type="button" class="btn btn-secondary btn-sm" disabled=move || checking.get() on:click=check_now>
                <Icon name="refresh" />
                {move || if checking.get() { "Checking…" } else { "Check now" }}
            </button>
        </PageHeader>
        <div class="stack">{content}</div>
        <ConfirmDialog open=confirm_open title="Install the update and restart?" message=confirm_message
            confirm_label="Install and restart" on_confirm=Callback::new(install) />
    }
}

#[component]
fn UpdatesView(
    s: UpdateStatusDto,
    on_settings: Callback<UpdateSettings>,
    on_install: Callback<String>,
) -> impl IntoView {
    let repo_url = format::http_url(&format!("https://github.com/{}/releases", s.repository));
    let how_to = match s.install {
        InstallKind::Docker => {
            "Pull the new image and recreate the container: docker compose pull && docker compose up -d. Your /config volume is kept."
        }
        InstallKind::Windows => {
            "Stop Reelix, download the Windows zip below and unzip it over your Reelix folder. The data folder is kept."
        }
        InstallKind::Other => {
            "Stop Reelix, replace the reelix binary and web folder with the ones from the release, and start it again."
        }
    };
    let su = s.self_update.clone();
    let busy = su.state.busy();
    let progress = busy.then(|| {
        let text = state_text(&su);
        view! {
            <div class="alert alert-info" role="status" aria-live="polite">
                <Spinner label=text.clone() />
                <div class="alert-body">
                    <strong>{text}</strong>
                    {su.backup.clone().map(|b| view! { <span>{format!("Backup made first: {b}")}</span> })}
                </div>
            </div>
        }
    });
    let failure = (su.state == SelfUpdateState::Failed).then(|| {
        view! {
            <div class="alert alert-danger" role="alert">
                <Icon name="warning" />
                <div class="alert-body">
                    <strong>{format!("Installing Reelix {} failed", su.version.clone().unwrap_or_default())}</strong>
                    <span>{su.error.clone().unwrap_or_default()}</span>
                    {su.backup.clone().map(|b| view! { <span>{format!("The backup made before the update: {b}")}</span> })}
                </div>
            </div>
        }
    });
    let headline = match (&s.latest, s.update_available) {
        (Some(l), true) if su.supported => {
            let version = l.version.clone();
            view! {
                <div class="alert alert-info" role="status">
                    <Icon name="download" />
                    <div class="alert-body">
                        <strong>{format!("Reelix {} is available", l.version)}</strong>
                        <span>"Reelix can install it and restart by itself; it backs up the database first."</span>
                        <div>
                            <button type="button" class="btn btn-primary btn-sm" disabled=busy
                                on:click=move |_| on_install.run(version.clone())>
                                <Icon name="download" />
                                "Install and restart"
                            </button>
                        </div>
                    </div>
                </div>
            }
            .into_any()
        }
        (Some(l), true) => view! {
            <div class="alert alert-info" role="status">
                <Icon name="download" />
                <div class="alert-body">
                    <strong>{format!("Reelix {} is available", l.version)}</strong>
                    <span>{how_to}</span>
                    {su.reason.clone().map(|r| view! { <span class="muted">{format!("Why it can't update itself: {r}.")}</span> })}
                </div>
            </div>
        }
        .into_any(),
        (Some(_), false) => view! {
            <div class="alert alert-success" role="status">
                <Icon name="check" />
                <div class="alert-body">
                    <strong>"Reelix is up to date"</strong>
                    <span>{format!("You're running the newest release, {}.", s.current_version)}</span>
                </div>
            </div>
        }
        .into_any(),
        (None, _) => ().into_any(),
    };
    let automatic = s.automatic;
    let auto_install = su.auto_install;
    let supported = su.supported;
    let checked = s
        .checked_at
        .map(|t| format!("Last checked {}", format::relative(t)))
        .unwrap_or_else(|| "Not checked yet".into());
    let hint = asset_hint(s.install);
    let version = s.current_version.clone();
    let version_label = s.current_version.clone();
    let repository = s.repository.clone();
    let repository_label = s.repository.clone();
    let releases = s.releases.clone();
    let auto_install_desc = if supported {
        "When the scheduled check finds a new release (not a pre-release), Reelix backs up the database, installs it and restarts."
    } else {
        "Not available for this install; see above for how to update."
    };

    view! {
        {s.error.clone().map(|e| view! {
            <div class="alert alert-warning" role="alert">
                <Icon name="warning" />
                <div class="alert-body">
                    <strong>"Couldn't check for updates"</strong>
                    <span>{e}</span>
                </div>
            </div>
        })}
        {progress}
        {failure}
        {headline}
        <Card title="Installed">
            <dl class="kv">
                <div class="kv-row"><dt>"Version"</dt><dd>{version_label}</dd></div>
                <div class="kv-row"><dt>"Releases from"</dt><dd>
                    <a href=repo_url target="_blank" rel="noopener" class="mono">{repository_label}</a>
                </dd></div>
                <div class="kv-row"><dt>"Checked"</dt><dd>{checked}</dd></div>
            </dl>
            <label class="checkbox toggle-row">
                <input type="checkbox" prop:checked=automatic
                    on:change=move |e| on_settings.run(UpdateSettings {
                        automatic: event_target_checked(&e),
                        auto_install,
                    }) />
                <span>
                    <span class="perm-label">"Check for updates automatically"</span>
                    <span class="perm-desc">"Twice a day; System → Health shows a notice when a new release is out. Updates are only installed when you choose to, or automatically if you turn that on below."</span>
                </span>
            </label>
            <label class="checkbox toggle-row">
                <input type="checkbox" prop:checked=auto_install disabled=!(supported && automatic)
                    on:change=move |e| on_settings.run(UpdateSettings {
                        automatic,
                        auto_install: event_target_checked(&e),
                    }) />
                <span>
                    <span class="perm-label">"Install updates automatically"</span>
                    <span class="perm-desc">{auto_install_desc}</span>
                </span>
            </label>
        </Card>
        <Card title="Releases">
            {if releases.is_empty() {
                view! {
                    <EmptyState icon="download" title="No releases published yet" compact=true
                        description=format!("Releases appear here once they are published on GitHub ({repository}).") />
                }
                .into_any()
            } else {
                view! {
                    <div class="release-list">
                        {releases
                            .into_iter()
                            .map(|r| {
                                let current = r.version == version;
                                view! { <Release r=r current=current hint=hint /> }
                            })
                            .collect_view()}
                    </div>
                }
                .into_any()
            }}
        </Card>
    }
}

#[component]
fn Release(r: ReleaseDto, current: bool, hint: Option<&'static str>) -> impl IntoView {
    let assets: Vec<_> = r
        .assets
        .iter()
        .filter(|a| hint.is_none_or(|h| a.name.to_ascii_lowercase().contains(h)))
        .cloned()
        .collect();
    let assets = if assets.is_empty() {
        r.assets.clone()
    } else {
        assets
    };
    let notes = r.notes.trim().to_string();
    view! {
        <article class="release" class:release-new=r.newer>
            <header class="release-head">
                <h3 class="release-title">{r.name.clone()}</h3>
                {r.newer.then(|| view! { <StatusBadge tone=Tone::Info label="New" /> })}
                {current.then(|| view! { <StatusBadge tone=Tone::Success label="Installed" /> })}
                {r.prerelease.then(|| view! { <StatusBadge tone=Tone::Warning label="Pre-release" /> })}
                <span class="muted release-date">
                    {r.published_at.map(format::date).unwrap_or_default()}
                </span>
                <a class="btn btn-sm btn-ghost" href=format::http_url(&r.url) target="_blank" rel="noopener">
                    <Icon name="external" />
                    "GitHub"
                </a>
            </header>
            {if notes.is_empty() {
                view! { <p class="muted">"No release notes."</p> }.into_any()
            } else {
                view! { <div class="release-notes">{notes}</div> }.into_any()
            }}
            {(!assets.is_empty()).then(|| view! {
                <div class="release-assets">
                    {assets
                        .into_iter()
                        .map(|a| view! {
                            <a class="btn btn-sm btn-secondary" href=format::http_url(&a.url) rel="noopener">
                                <Icon name="download" />
                                {format!("{} ({})", a.name, bytes(a.size_bytes))}
                            </a>
                        })
                        .collect_view()}
                </div>
            })}
        </article>
    }
}
