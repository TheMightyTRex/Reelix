//! `/settings/indexers` — RSS interval, private-address policy, FlareSolverr, proxy and the
//! indexer definition catalog (runtime sync status, auto-update, load failures).

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::providers::{
    DefinitionSource, DefinitionsSettingsDto, DefinitionsStatusDto, IndexerSettingsDto, ProxyDto,
};
use reelix_core::DomainEvent;

use crate::api;
use crate::components::{Card, CopyButton, ErrorBox, Icon, PageHeader, Skeleton, Spinner, StatusBadge, Tone};
use crate::format;
use crate::pages::system::require_admin;
use crate::state::Toasts;
use crate::ws::LiveEvents;

#[component]
pub fn IndexerSettingsPage() -> impl IntoView {
    require_admin();
    let settings = LocalResource::new(|| api::get::<IndexerSettingsDto>("/settings/indexers"));
    view! {
        <PageHeader title="Indexer settings" subtitle="Options shared by every indexer. Manage the indexers themselves under Indexers." />
        <p class="field-hint"><A href="/indexers">"Go to Indexers →"</A></p>
        {move || match settings.get() {
            None => view! { <Skeleton lines=5 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) /> }.into_any(),
            Some(Ok(s)) => view! { <IndexerSettingsForm initial=s /> }.into_any(),
        }}
        <DefinitionsCard />
    }
}

#[component]
fn IndexerSettingsForm(initial: IndexerSettingsDto) -> impl IntoView {
    let toasts = Toasts::expect();
    let rss = RwSignal::new(initial.rss_interval_minutes.to_string());
    let private = RwSignal::new(initial.allow_private_addresses);
    let flare = RwSignal::new(initial.flaresolverr_url.clone().unwrap_or_default());
    let p = initial.proxy.clone();
    let proxy_kind = RwSignal::new(p.as_ref().map_or("http".to_owned(), |p| p.kind.clone()));
    let proxy_host = RwSignal::new(p.as_ref().map(|p| p.host.clone()).unwrap_or_default());
    let proxy_port = RwSignal::new(p.as_ref().map(|p| p.port.to_string()).unwrap_or_default());
    let proxy_user = RwSignal::new(p.as_ref().and_then(|p| p.username.clone()).unwrap_or_default());
    let proxy_pass = RwSignal::new(p.and_then(|p| p.password).unwrap_or_default());
    let saving = RwSignal::new(false);

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let host = proxy_host.get_untracked().trim().to_owned();
        let body = IndexerSettingsDto {
            rss_interval_minutes: rss.get_untracked().trim().parse().unwrap_or(15),
            allow_private_addresses: private.get_untracked(),
            flaresolverr_url: Some(flare.get_untracked()).filter(|s| !s.trim().is_empty()),
            proxy: (!host.is_empty()).then(|| ProxyDto {
                kind: proxy_kind.get_untracked(),
                host,
                port: proxy_port.get_untracked().trim().parse().unwrap_or(0),
                username: Some(proxy_user.get_untracked()).filter(|s| !s.is_empty()),
                password: Some(proxy_pass.get_untracked()).filter(|s| !s.is_empty()),
            }),
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, IndexerSettingsDto>("/settings/indexers", &body).await {
                Ok(s) => {
                    proxy_pass.set(s.proxy.and_then(|p| p.password).unwrap_or_default());
                    toasts.success("Indexer settings saved.");
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    view! {
        <form class="form" on:submit=save>
            <Card title="General">
                <div class="field">
                    <label for="is-rss">"RSS sync interval (minutes)"</label>
                    <input id="is-rss" class="input" type="number" min="0" max="120"
                        prop:value=move || rss.get() on:input=move |e| rss.set(event_target_value(&e)) />
                    <p class="field-hint">"0 disables RSS sync; otherwise 10–120."</p>
                </div>
                <div class="field field-check">
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || private.get() on:change=move |e| private.set(event_target_checked(&e)) />
                        "Allow private / LAN addresses"
                    </label>
                    <p class="field-hint">"Lets indexers, notifications and import lists reach loopback and private networks (e.g. a local Prowlarr or Jackett). Download clients and media servers are always allowed."</p>
                </div>
                <div class="field">
                    <label for="is-flare">"FlareSolverr URL"</label>
                    <input id="is-flare" class="input" type="url" placeholder="http://flaresolverr:8191"
                        prop:value=move || flare.get() on:input=move |e| flare.set(event_target_value(&e)) />
                    <p class="field-hint">"Used for Cloudflare-protected definition indexers."</p>
                </div>
            </Card>
            <Card title="Proxy" subtitle="Route indexer traffic through an HTTP or SOCKS5 proxy. Leave the host empty to disable.">
                <div class="field-row">
                    <div class="field">
                        <label for="px-kind">"Type"</label>
                        <select id="px-kind" class="input" on:change=move |e| proxy_kind.set(event_target_value(&e))>
                            <option value="http" selected=move || proxy_kind.get() == "http">"HTTP"</option>
                            <option value="socks5" selected=move || proxy_kind.get() == "socks5">"SOCKS5"</option>
                        </select>
                    </div>
                    <div class="field">
                        <label for="px-host">"Host"</label>
                        <input id="px-host" class="input" prop:value=move || proxy_host.get() on:input=move |e| proxy_host.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="px-port">"Port"</label>
                        <input id="px-port" class="input" type="number" min="1" max="65535"
                            prop:value=move || proxy_port.get() on:input=move |e| proxy_port.set(event_target_value(&e)) />
                    </div>
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="px-user">"Username"</label>
                        <input id="px-user" class="input" autocomplete="off" prop:value=move || proxy_user.get() on:input=move |e| proxy_user.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="px-pass">"Password"</label>
                        <input id="px-pass" class="input" type="password" autocomplete="new-password"
                            prop:value=move || proxy_pass.get() on:input=move |e| proxy_pass.set(event_target_value(&e)) />
                    </div>
                </div>
            </Card>
            <div class="form-actions">
                <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                    {move || if saving.get() { "Saving…" } else { "Save" }}
                </button>
            </div>
        </form>
    }
}

// ---------------- indexer definition catalog ----------------

/// "Definitions" card: catalog status, auto-update settings, "Sync now" and load failures.
#[component]
fn DefinitionsCard() -> impl IntoView {
    let toasts = Toasts::expect();
    let live = LiveEvents::expect();
    let status = LocalResource::new(|| api::get::<DefinitionsStatusDto>("/indexers/definitions/status"));
    let settings = LocalResource::new(|| api::get::<DefinitionsSettingsDto>("/settings/definitions"));
    // A sync started from here (or seen running via live events).
    let syncing = RwSignal::new(false);
    let requested = RwSignal::new(false);

    Effect::new(move |prev: Option<u64>| {
        let latest = live.latest();
        let seq = latest.as_ref().map_or(0, |e| e.seq);
        if let (Some(prev), Some(ev)) = (prev, latest)
            && ev.seq != prev
            && let DomainEvent::EntityChanged { entity, action, .. } = &ev.event
        {
            match (entity.as_str(), action.as_str()) {
                ("indexer_definitions", "syncing") => syncing.set(true),
                ("indexer_definitions", outcome @ ("synced" | "failed")) => {
                    syncing.set(false);
                    if requested.get_untracked() {
                        requested.set(false);
                        if outcome == "synced" {
                            toasts.success("Indexer definitions are up to date.");
                        } else {
                            toasts.error("Updating indexer definitions failed; see the error below.");
                        }
                    }
                    status.refetch();
                }
                ("indexer_definitions", _) | ("definitions_settings", _) => status.refetch(),
                _ => {}
            }
        }
        seq
    });

    let sync_now = move |_| {
        syncing.set(true);
        requested.set(true);
        spawn_local(async move {
            if let Err(e) = api::post_empty::<EnqueuedJob>("/indexers/definitions/sync").await {
                syncing.set(false);
                requested.set(false);
                toasts.error(format!("Could not start the sync: {e}"));
            }
        });
    };

    let actions = view! {
        <button type="button" class="btn btn-secondary btn-sm" on:click=sync_now
            disabled=move || syncing.get() || status.get().and_then(Result::ok).is_some_and(|s| s.syncing)>
            <Icon name="refresh" />
            {move || if syncing.get() { "Syncing…" } else { "Sync now" }}
        </button>
    }
    .into_any();

    view! {
        <Card title="Definitions" class="defs-card"
            subtitle="Community indexer definitions are downloaded at runtime and kept up to date; Reelix ships none."
            actions=actions>
            {move || syncing.get().then(|| view! {
                <div class="defs-progress" role="status"><Spinner label="Downloading and loading definitions…" /></div>
            })}
            {move || match status.get() {
                None => view! { <Skeleton lines=4 /> }.into_any(),
                Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| status.refetch()) /> }.into_any(),
                Some(Ok(st)) => view! { <DefinitionsStatusView status=st /> }.into_any(),
            }}
            {move || match settings.get() {
                Some(Ok(s)) => view! { <DefinitionsSettingsForm initial=s /> }.into_any(),
                Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) /> }.into_any(),
                None => ().into_any(),
            }}
        </Card>
    }
}

#[component]
fn DefinitionsStatusView(status: DefinitionsStatusDto) -> impl IntoView {
    let st = status;
    let failed_count = st.failed.len();
    let custom_dir = st.custom_dir.clone();
    let when = |t: Option<chrono::DateTime<chrono::Utc>>| match t {
        Some(t) => view! { <span title=format::datetime(t)>{format::relative(t)}</span> }.into_any(),
        None => view! { <span class="muted">"Never"</span> }.into_any(),
    };
    let summary = if st.loaded == 0 {
        (Tone::Warning, "No definitions loaded".to_owned())
    } else {
        (Tone::Success, format!("{} loaded", st.loaded))
    };
    let failed = st.failed.clone();
    view! {
        {st.last_error.clone().map(|e| view! {
            <div class="alert alert-danger" role="alert">
                <Icon name="warning" />
                <div class="alert-body">
                    <strong>"The last update failed"</strong>
                    <span>{e}</span>
                </div>
            </div>
        })}
        <dl class="kv">
            <div class="kv-row"><dt>"Status"</dt><dd><StatusBadge tone=summary.0 label=summary.1 dot=true /></dd></div>
            <div class="kv-row"><dt>"Source"</dt><dd class="defs-source">
                <span class="mono">{st.source_url.clone()}</span>
                <span class="chip">{format!("schema {}", st.schema_version)}</span>
                {st.commit.clone().map(|c| view! { <span class="chip mono" title=c.clone()>{c.chars().take(7).collect::<String>()}</span> })}
            </dd></div>
            <div class="kv-row"><dt>"Last update"</dt><dd>{when(st.last_sync)}</dd></div>
            <div class="kv-row"><dt>"Last check"</dt><dd>{when(st.last_check)}</dd></div>
            <div class="kv-row"><dt>"Definitions"</dt><dd class="defs-counts">
                <span>{format!("{} downloaded", st.upstream_count)}</span>
                <span>{format!("{} custom", st.custom_count)}</span>
                {(st.overridden > 0).then(|| view! { <span>{format!("{} overridden by custom", st.overridden)}</span> })}
                <span class:defs-failed={failed_count > 0}>{format!("{failed_count} failed to load")}</span>
            </dd></div>
            <div class="kv-row"><dt>"Custom folder"</dt><dd class="defs-source">
                <span class="mono">{custom_dir.clone()}</span>
                <CopyButton text=Signal::derive(move || custom_dir.clone()) />
            </dd></div>
        </dl>
        <p class="field-hint">
            "Put your own .yml definitions in the custom folder; a custom definition with the same id replaces the downloaded one. "
            "Downloaded files are replaced on every update — don't edit them."
        </p>
        {(failed_count > 0).then(|| view! {
            <details class="defs-failures">
                <summary>{format!("{failed_count} definitions failed to load")}</summary>
                <ul class="defs-failure-list">
                    {failed.into_iter().map(|f| view! {
                        <li>
                            <div class="defs-failure-head">
                                <span class="mono">{f.file}</span>
                                <span class="chip">{match f.source { DefinitionSource::Custom => "custom", DefinitionSource::Upstream => "downloaded" }}</span>
                            </div>
                            <pre class="defs-failure-error">{f.error}</pre>
                        </li>
                    }).collect_view()}
                </ul>
            </details>
        })}
    }
}

#[component]
fn DefinitionsSettingsForm(initial: DefinitionsSettingsDto) -> impl IntoView {
    let toasts = Toasts::expect();
    let enabled = RwSignal::new(initial.enabled);
    let hours = RwSignal::new(initial.auto_update_hours.to_string());
    let source = RwSignal::new(initial.source_url.clone());
    let version = RwSignal::new(initial.schema_version.clone());
    let saving = RwSignal::new(false);
    let base = StoredValue::new(initial);

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let mut body = base.get_value();
        body.enabled = enabled.get_untracked();
        body.auto_update_hours = hours.get_untracked().trim().parse().unwrap_or(0);
        body.source_url = source.get_untracked().trim().to_owned();
        body.schema_version = version.get_untracked().trim().to_owned();
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, DefinitionsSettingsDto>("/settings/definitions", &body).await {
                Ok(s) => {
                    base.set_value(s);
                    toasts.success("Definition settings saved.");
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    view! {
        <form class="form defs-form" on:submit=save>
            <div class="field-row">
                <div class="field field-check">
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || enabled.get() on:change=move |e| enabled.set(event_target_checked(&e)) />
                        "Update automatically"
                    </label>
                </div>
                <div class="field">
                    <label for="defs-hours">"Check every (hours)"</label>
                    <input id="defs-hours" class="input" type="number" min="1" max="720" disabled=move || !enabled.get()
                        prop:value=move || hours.get() on:input=move |e| hours.set(event_target_value(&e)) />
                </div>
            </div>
            <details class="field">
                <summary>"Advanced"</summary>
                <div class="field">
                    <label for="defs-src">"Source archive URL"</label>
                    <input id="defs-src" class="input" type="url"
                        prop:value=move || source.get() on:input=move |e| source.set(event_target_value(&e)) />
                    <p class="field-hint">"A .tar.gz of a definitions repository (http or https)."</p>
                </div>
                <div class="field">
                    <label for="defs-ver">"Schema version"</label>
                    <input id="defs-ver" class="input" placeholder="v11"
                        prop:value=move || version.get() on:input=move |e| version.set(event_target_value(&e)) />
                    <p class="field-hint">"Only definitions/<version>/*.yml are installed from the archive."</p>
                </div>
            </details>
            <div class="form-actions">
                <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                    {move || if saving.get() { "Saving…" } else { "Save" }}
                </button>
            </div>
        </form>
    }
}
