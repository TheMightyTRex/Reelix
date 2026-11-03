//! `/system/updates` — installed version, new releases from GitHub and their release notes.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::system::{InstallKind, ReleaseDto, UpdateSettings, UpdateStatusDto};

use super::require_admin;
use crate::api;
use crate::components::library_common::bytes;
use crate::components::{Card, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge, Tone};
use crate::format;
use crate::state::Toasts;

/// Release asset name fragment for the platform of this install.
fn asset_hint(install: InstallKind) -> Option<&'static str> {
    match install {
        InstallKind::Windows => Some("windows"),
        _ => None,
    }
}

#[component]
pub fn UpdatesPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let status = LocalResource::new(|| api::get::<UpdateStatusDto>("/system/updates"));
    // Set after "Check now" so the page shows the fresh result without a second request.
    let fresh = RwSignal::new(None::<UpdateStatusDto>);
    let checking = RwSignal::new(false);

    let current = move || fresh.get().map(Ok).or_else(|| status.get());

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

    let set_automatic = move |on: bool| {
        spawn_local(async move {
            match api::put::<_, UpdateStatusDto>(
                "/system/updates/settings",
                &UpdateSettings { automatic: on },
            )
            .await
            {
                Ok(s) => {
                    toasts.success(if on {
                        "Reelix will check for updates twice a day."
                    } else {
                        "Automatic update checks are off."
                    });
                    fresh.set(Some(s));
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };

    let content = move || match current() {
        None => view! { <Card><Skeleton lines=6 /></Card> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| status.refetch()) /> }.into_any()
        }
        Some(Ok(s)) => view! { <UpdatesView s=s on_automatic=Callback::new(set_automatic) /> }.into_any(),
    };

    view! {
        <PageHeader title="Updates" subtitle="New Reelix releases and what changed.">
            <button type="button" class="btn btn-secondary btn-sm" disabled=move || checking.get() on:click=check_now>
                <Icon name="refresh" />
                {move || if checking.get() { "Checking…" } else { "Check now" }}
            </button>
        </PageHeader>
        <div class="stack">{content}</div>
    }
}

#[component]
fn UpdatesView(s: UpdateStatusDto, on_automatic: Callback<bool>) -> impl IntoView {
    let repo_url = format!("https://github.com/{}/releases", s.repository);
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
    let headline = match (&s.latest, s.update_available) {
        (Some(l), true) => view! {
            <div class="alert alert-info" role="status">
                <Icon name="download" />
                <div class="alert-body">
                    <strong>{format!("Reelix {} is available", l.version)}</strong>
                    <span>{how_to}</span>
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
                    on:change=move |e| on_automatic.run(event_target_checked(&e)) />
                <span>
                    <span class="perm-label">"Check for updates automatically"</span>
                    <span class="perm-desc">"Twice a day; System → Health shows a notice when a new release is out. Reelix never installs updates by itself."</span>
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
