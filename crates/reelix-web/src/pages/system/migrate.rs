//! `/system/migrate`: import settings and libraries from Radarr, Sonarr, Prowlarr, Readarr,
//! Lidarr and Jellyseerr. Connect → preview → run, with live progress.

use std::collections::{BTreeMap, BTreeSet};

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::migrate::{
    MigrateAction, MigrateCategory, MigrateConnection, MigrateItem, MigratePlan, MigrateRunDto,
    MigrateRunRequest, MigrateState, PathRewrite, PendingMigrationSource, SAVED_API_KEY, SourceKind,
};
use serde::Deserialize;

use crate::api;
use crate::components::{Card, DataTable, EmptyState, Icon, PageHeader, Spinner, StatusBadge, Tone};
use crate::pages::system::require_admin;
use crate::state::Toasts;

#[derive(Deserialize)]
struct TestResult {
    version: String,
}

#[derive(Clone, Default)]
struct RewriteRow {
    from: RwSignal<String>,
    to: RwSignal<String>,
}

#[component]
pub fn MigratePage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let source = RwSignal::new(SourceKind::Radarr);
    let url = RwSignal::new(String::new());
    let api_key = RwSignal::new(String::new());
    let rewrites = RwSignal::new(Vec::<RewriteRow>::new());
    let secrets = RwSignal::new(BTreeMap::<String, String>::new());
    let busy = RwSignal::new(None::<&'static str>);
    let error = RwSignal::new(None::<String>);
    let connected = RwSignal::new(None::<String>);
    let plan = RwSignal::new(None::<MigratePlan>);
    let chosen = RwSignal::new(BTreeSet::<MigrateCategory>::new());
    let search_missing = RwSignal::new(false);
    let run = RwSignal::new(None::<MigrateRunDto>);
    // Apps Setup found on this PC (the Windows installer's "Migration" option).
    let saved = RwSignal::new(Vec::<PendingMigrationSource>::new());

    // Show a run still going (or just finished) when the page opens.
    spawn_local(async move {
        if let Ok(Some(r)) = api::get::<Option<MigrateRunDto>>("/system/migrate").await {
            run.set(Some(r));
        }
    });
    // Poll while a run is in progress.
    Effect::new(move |_| {
        let running = run.with(|r| r.as_ref().is_some_and(|r| r.state == MigrateState::Running));
        if running {
            spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(1000).await;
                if let Ok(r) = api::get::<Option<MigrateRunDto>>("/system/migrate").await {
                    run.set(r);
                }
            });
        }
    });

    let connection = move || MigrateConnection {
        source: source.get_untracked(),
        url: url.get_untracked().trim().to_owned(),
        api_key: api_key.get_untracked().trim().to_owned(),
        path_rewrites: rewrites
            .get_untracked()
            .iter()
            .map(|r| PathRewrite {
                from: r.from.get_untracked().trim().to_owned(),
                to: r.to.get_untracked().trim().to_owned(),
            })
            .filter(|r| !r.from.is_empty() && !r.to.is_empty())
            .collect(),
        secrets: secrets.get_untracked(),
    };
    let reset = move || {
        connected.set(None);
        plan.set(None);
        error.set(None);
    };
    let use_saved = move |p: &PendingMigrationSource| {
        source.set(p.source);
        url.set(p.url.clone());
        api_key.set(SAVED_API_KEY.to_owned());
        reset();
    };
    let load_saved = move |prefill: bool| {
        spawn_local(async move {
            if let Ok(list) = api::get::<Vec<PendingMigrationSource>>("/system/migrate/pending").await {
                if prefill
                    && url.get_untracked().trim().is_empty()
                    && let Some(first) = list.first()
                {
                    use_saved(first);
                }
                saved.set(list);
            }
        });
    };
    load_saved(true);
    let forget = move |kind: SourceKind| {
        spawn_local(async move {
            match api::delete(&format!("/system/migrate/pending/{}", kind.as_str())).await {
                Ok(()) => {
                    if source.get_untracked() == kind && api_key.get_untracked() == SAVED_API_KEY {
                        api_key.set(String::new());
                        reset();
                    }
                    load_saved(false);
                }
                Err(e) => toasts.error(e.message),
            }
        });
    };

    let test = move |_| {
        error.set(None);
        busy.set(Some("test"));
        let c = connection();
        spawn_local(async move {
            match api::post::<_, TestResult>("/system/migrate/test", &c).await {
                Ok(r) => connected.set(Some(r.version)),
                Err(e) => {
                    connected.set(None);
                    error.set(Some(e.message));
                }
            }
            busy.set(None);
        });
    };
    let preview = move |_| {
        error.set(None);
        busy.set(Some("preview"));
        let c = connection();
        spawn_local(async move {
            match api::post::<_, MigratePlan>("/system/migrate/preview", &c).await {
                Ok(p) => {
                    connected.set(Some(p.version.clone()));
                    let cats: BTreeSet<MigrateCategory> = p
                        .items
                        .iter()
                        .filter(|i| i.action == MigrateAction::Create)
                        .map(|i| i.category)
                        .collect();
                    chosen.set(cats);
                    plan.set(Some(p));
                }
                Err(e) => error.set(Some(e.message)),
            }
            busy.set(None);
        });
    };
    let start = move |_| {
        error.set(None);
        busy.set(Some("run"));
        let req = MigrateRunRequest {
            connection: connection(),
            categories: chosen.get_untracked().into_iter().collect(),
            search_missing: search_missing.get_untracked(),
        };
        spawn_local(async move {
            match api::post::<_, MigrateRunDto>("/system/migrate/run", &req).await {
                Ok(r) => {
                    toasts.success("Migration started.");
                    plan.set(None);
                    run.set(Some(r));
                    // A saved key is used up once the run has it.
                    if api_key.get_untracked() == SAVED_API_KEY {
                        api_key.set(String::new());
                        load_saved(false);
                    }
                }
                Err(e) => error.set(Some(e.message)),
            }
            busy.set(None);
        });
    };

    let source_select = move || {
        SourceKind::ALL
            .into_iter()
            .map(|k| view! { <option value=k.as_str() selected=move || source.get() == k>{k.label()}</option> })
            .collect_view()
    };

    view! {
        <PageHeader title="Migrate" subtitle="Bring settings and libraries over from Radarr, Sonarr, Prowlarr, Readarr, Lidarr and Jellyseerr." />
        <div class="stack">
            {move || run.get().map(|r| view! { <RunCard run=r on_close=Callback::new(move |()| run.set(None)) /> })}
            {move || {
                let list = saved.get();
                (!list.is_empty()).then(|| view! {
                    <Card title="Found by Setup" subtitle="Setup found these apps on this PC and saved their address and API key. Pick one, preview the migration and start it when the preview looks right; nothing has been imported yet.">
                        <DataTable caption="Apps found by Setup" headers=vec!["App", "URL", ""]>
                            {list
                                .into_iter()
                                .map(|p| view! { <SavedRow saved=p source=source url=url api_key=api_key on_use=Callback::new(move |p: PendingMigrationSource| use_saved(&p)) on_forget=Callback::new(forget) /> })
                                .collect_view()}
                        </DataTable>
                    </Card>
                })
            }}
            <Card title="1. Connect" subtitle="Reelix reads the other app through its API; nothing is changed there. Find the API key under Settings → General in that app.">
                <div class="form">
                    <div class="field-row">
                        <div class="field">
                            <label for="mg-source">"App"</label>
                            <select id="mg-source" class="input"
                                on:change=move |e| {
                                    if let Some(k) = SourceKind::parse(&event_target_value(&e)) {
                                        source.set(k);
                                        reset();
                                    }
                                }>
                                {source_select}
                            </select>
                        </div>
                        <div class="field grow">
                            <label for="mg-url">"URL"</label>
                            <input id="mg-url" class="input mono" type="url"
                                placeholder=move || format!("http://localhost:{}", source.get().default_port())
                                prop:value=move || url.get()
                                on:input=move |e| { url.set(event_target_value(&e)); reset(); } />
                            <span class="field-hint">"Include the URL base if it has one, e.g. http://nas:7878/radarr."</span>
                        </div>
                    </div>
                    <div class="field">
                        <label for="mg-key">"API key"</label>
                        <input id="mg-key" class="input mono" type="password" autocomplete="off"
                            prop:value=move || api_key.get()
                            on:input=move |e| { api_key.set(event_target_value(&e)); reset(); } />
                        {move || (api_key.get() == SAVED_API_KEY).then(|| view! {
                            <span class="field-hint">"Using the API key Setup saved. Type a key to use another one."</span>
                        })}
                    </div>
                    <PathRewrites rows=rewrites />
                    {move || error.get().map(|e| view! { <p class="field-error" role="alert">{e}</p> })}
                    {move || connected.get().map(|v| view! {
                        <p class="field-hint" role="status">
                            <StatusBadge tone=Tone::Success label="Connected" dot=true />
                            {format!(" {} {v}", source.get().label())}
                        </p>
                    })}
                    <div class="form-actions">
                        <button type="button" class="btn btn-secondary" on:click=test
                            disabled=move || busy.get().is_some() || url.get().trim().is_empty() || api_key.get().trim().is_empty()>
                            {move || if busy.get() == Some("test") { "Testing…" } else { "Test connection" }}
                        </button>
                        <button type="button" class="btn btn-primary" on:click=preview
                            disabled=move || busy.get().is_some() || url.get().trim().is_empty() || api_key.get().trim().is_empty()>
                            {move || if busy.get() == Some("preview") { "Reading…" } else { "Preview migration" }}
                        </button>
                    </div>
                </div>
            </Card>
            {move || {
                if busy.get() == Some("preview") {
                    return view! { <Card title="2. Review"><Spinner label="Reading the app…" /></Card> }.into_any();
                }
                match plan.get() {
                    None => ().into_any(),
                    Some(p) => view! {
                        <PlanCard plan=p chosen=chosen secrets=secrets search_missing=search_missing />
                        <Card title="3. Migrate">
                            <p class="field-hint">
                                "Items already in Reelix are reused, so you can run a migration again later (for example after adding a path rewrite or a TMDB key). "
                                "Files on disk are not moved or renamed."
                            </p>
                            <div class="form-actions">
                                <button type="button" class="btn btn-primary" on:click=start
                                    disabled=move || busy.get().is_some() || chosen.with(BTreeSet::is_empty)
                                        || run.with(|r| r.as_ref().is_some_and(|r| r.state == MigrateState::Running))>
                                    <Icon name="download" />
                                    {move || if busy.get() == Some("run") { "Starting…" } else { "Start migration" }}
                                </button>
                            </div>
                        </Card>
                    }
                    .into_any(),
                }
            }}
        </div>
    }
}

/// One app Setup saved: "Use" fills in the connection form (with the saved key masked).
#[component]
fn SavedRow(
    saved: PendingMigrationSource,
    source: RwSignal<SourceKind>,
    url: RwSignal<String>,
    api_key: RwSignal<String>,
    on_use: Callback<PendingMigrationSource>,
    on_forget: Callback<SourceKind>,
) -> impl IntoView {
    let kind = saved.source;
    let saved_url = saved.url.clone();
    let selected = Memo::new(move |_| {
        source.get() == kind && url.with(|u| *u == saved_url) && api_key.with(|k| k == SAVED_API_KEY)
    });
    let label = saved.url.clone();
    view! {
        <tr>
            <td><span class="cell-strong">{kind.label()}</span></td>
            <td><span class="mono">{label}</span></td>
            <td>
                <div class="lib-toolbar">
                    <button type="button" class="btn btn-sm btn-secondary"
                        disabled=move || selected.get()
                        on:click=move |_| on_use.run(saved.clone())>
                        {move || if selected.get() { "Selected" } else { "Use" }}
                    </button>
                    <button type="button" class="btn btn-sm btn-ghost"
                        aria-label=format!("Forget {}", kind.label())
                        on:click=move |_| on_forget.run(kind)>
                        <Icon name="trash" />
                    </button>
                </div>
            </td>
        </tr>
    }
}

#[component]
fn PathRewrites(rows: RwSignal<Vec<RewriteRow>>) -> impl IntoView {
    view! {
        <fieldset class="field">
            <legend>"Path rewrites " <span class="optional">"(optional)"</span></legend>
            <span class="field-hint">
                "If Reelix sees your media under a different path than the other app (different Docker volumes, another computer), map the other app's path to the path Reelix uses."
            </span>
            {move || {
                rows.get()
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| {
                        view! {
                            <div class="lib-toolbar">
                                <input class="input grow mono" placeholder="/data/movies" aria-label=format!("Path in the other app, row {}", i + 1)
                                    prop:value=move || r.from.get() on:input=move |e| r.from.set(event_target_value(&e)) />
                                <span aria-hidden="true">"→"</span>
                                <input class="input grow mono" placeholder="/mnt/media/movies" aria-label=format!("Path Reelix uses, row {}", i + 1)
                                    prop:value=move || r.to.get() on:input=move |e| r.to.set(event_target_value(&e)) />
                                <button type="button" class="btn btn-sm btn-ghost" aria-label=format!("Remove rewrite {}", i + 1)
                                    on:click=move |_| rows.update(|v| { v.remove(i); })>
                                    <Icon name="trash" />
                                </button>
                            </div>
                        }
                    })
                    .collect_view()
            }}
            <div>
                <button type="button" class="btn btn-sm btn-secondary"
                    on:click=move |_| rows.update(|v| v.push(RewriteRow::default()))>
                    <Icon name="plus" />
                    "Add path rewrite"
                </button>
            </div>
        </fieldset>
    }
}

fn action_badge(a: MigrateAction) -> impl IntoView {
    let (tone, label) = match a {
        MigrateAction::Create => (Tone::Success, "Add"),
        MigrateAction::Existing => (Tone::Neutral, "Already in Reelix"),
        MigrateAction::Skip => (Tone::Warning, "Skip"),
        MigrateAction::Failed => (Tone::Danger, "Failed"),
    };
    view! { <StatusBadge tone=tone label=label /> }
}

/// Items grouped by category, in walk order.
fn grouped(items: &[MigrateItem]) -> Vec<(MigrateCategory, Vec<MigrateItem>)> {
    let mut out: Vec<(MigrateCategory, Vec<MigrateItem>)> = Vec::new();
    for i in items {
        match out.iter_mut().find(|(c, _)| *c == i.category) {
            Some((_, v)) => v.push(i.clone()),
            None => out.push((i.category, vec![i.clone()])),
        }
    }
    out
}

fn counts(items: &[MigrateItem]) -> String {
    let n = |a| items.iter().filter(|i| i.action == a).count();
    let mut parts = Vec::new();
    for (a, label) in [
        (MigrateAction::Create, "to add"),
        (MigrateAction::Existing, "already in Reelix"),
        (MigrateAction::Skip, "skipped"),
        (MigrateAction::Failed, "failed"),
    ] {
        let c = n(a);
        if c > 0 {
            parts.push(format!("{c} {label}"));
        }
    }
    parts.join(" · ")
}

#[component]
fn PlanCard(
    plan: MigratePlan,
    chosen: RwSignal<BTreeSet<MigrateCategory>>,
    secrets: RwSignal<BTreeMap<String, String>>,
    search_missing: RwSignal<bool>,
) -> impl IntoView {
    let groups = grouped(&plan.items);
    let has_library = plan.items.iter().any(|i| {
        matches!(
            i.category,
            MigrateCategory::Movie
                | MigrateCategory::Series
                | MigrateCategory::Author
                | MigrateCategory::Artist
        )
    });
    let needs_secrets = plan.items.iter().any(|i| !i.missing_secrets.is_empty());
    view! {
        <Card title="2. Review" subtitle=format!("{} {} · {}", plan.source.label(), plan.version, counts(&plan.items))>
            {(!plan.notes.is_empty()).then(|| view! {
                <div class="alert alert-info" role="note">
                    <Icon name="info" />
                    <div class="alert-body">
                        {plan.notes.iter().map(|n| view! { <span>{n.clone()}</span> }).collect_view()}
                    </div>
                </div>
            })}
            {needs_secrets.then(|| view! {
                <div class="alert alert-warning" role="note">
                    <Icon name="warning" />
                    <div class="alert-body">
                        <strong>"Some passwords or API keys are hidden"</strong>
                        <span>"The other app doesn't reveal them. Enter them below and preview again to include those items."</span>
                    </div>
                </div>
            })}
            {if groups.is_empty() {
                view! { <EmptyState icon="info" title="Nothing to migrate" compact=true /> }.into_any()
            } else {
                groups
                    .into_iter()
                    .map(|(cat, items)| {
                        let creatable = items.iter().any(|i| i.action == MigrateAction::Create);
                        let summary = counts(&items);
                        view! {
                            <section class="migrate-group" aria-label=cat.label()>
                                <label class="checkbox">
                                    <input type="checkbox" disabled=!creatable
                                        prop:checked=move || chosen.with(|c| c.contains(&cat))
                                        on:change=move |e| {
                                            let on = event_target_checked(&e);
                                            chosen.update(|c| { if on { c.insert(cat); } else { c.remove(&cat); } });
                                        } />
                                    <strong>{cat.label()}</strong>
                                    <span class="muted">{format!(" {summary}")}</span>
                                </label>
                                <DataTable caption=cat.label() headers=vec!["Name", "Action", "Notes"]>
                                    {items.into_iter().map(|i| view! { <ItemRow item=i secrets=secrets /> }).collect_view()}
                                </DataTable>
                            </section>
                        }
                    })
                    .collect_view()
                    .into_any()
            }}
            {has_library.then(|| view! {
                <label class="checkbox toggle-row">
                    <input type="checkbox" prop:checked=move || search_missing.get()
                        on:change=move |e| search_missing.set(event_target_checked(&e)) />
                    <span>
                        <span class="perm-label">"Search for missing items after adding them"</span>
                        <span class="perm-desc">"Leave this off to keep indexers quiet; existing files are always scanned."</span>
                    </span>
                </label>
            })}
        </Card>
    }
}

#[component]
fn ItemRow(item: MigrateItem, secrets: RwSignal<BTreeMap<String, String>>) -> impl IntoView {
    let fields = item.missing_secrets.clone();
    let key = item.key.clone();
    view! {
        <tr>
            <td><span class="cell-strong">{item.name.clone()}</span></td>
            <td>{action_badge(item.action)}</td>
            <td>
                {item.detail.clone().map(|d| view! { <span class="cell-sub">{d}</span> })}
                {fields
                    .into_iter()
                    .map(|f| {
                        let k = format!("{key}:{f}");
                        let k2 = k.clone();
                        view! {
                            <input class="input input-sm mono" type="password" autocomplete="off"
                                placeholder=f.clone() aria-label=format!("{} for {}", f, item.name)
                                prop:value=move || secrets.with(|s| s.get(&k).cloned().unwrap_or_default())
                                on:input=move |e| {
                                    let v = event_target_value(&e);
                                    secrets.update(|s| { s.insert(k2.clone(), v); });
                                } />
                        }
                    })
                    .collect_view()}
            </td>
        </tr>
    }
}

#[component]
fn RunCard(run: MigrateRunDto, on_close: Callback<()>) -> impl IntoView {
    let pct = (run.done * 100).checked_div(run.total).unwrap_or(0).min(100);
    let (tone, label) = match run.state {
        MigrateState::Running => (Tone::Info, "Running"),
        MigrateState::Finished => (Tone::Success, "Finished"),
        MigrateState::Failed => (Tone::Danger, "Failed"),
    };
    let title = format!("Migration from {}", run.source.label());
    let groups = grouped(&run.items);
    let running = run.state == MigrateState::Running;
    view! {
        <Card title=title subtitle=counts(&run.items)
            actions=view! {
                <StatusBadge tone=tone label=label dot=true />
                {(!running).then(|| view! {
                    <button type="button" class="btn btn-sm btn-ghost" aria-label="Dismiss" on:click=move |_| on_close.run(())>
                        <Icon name="close" />
                    </button>
                })}
            }
            .into_any()>
            {running.then(|| view! {
                <div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow=pct
                    aria-label="Migration progress">
                    <div class="progress-bar" style=format!("width: {pct}%")></div>
                </div>
                <p class="field-hint" aria-live="polite">
                    {format!("{} of {}", run.done, run.total)}
                    {run.current.clone().map(|c| format!(" · {c}"))}
                </p>
            })}
            {run.error.clone().map(|e| view! { <p class="field-error" role="alert">{e}</p> })}
            {(run.state == MigrateState::Finished && run.source != SourceKind::Jellyseerr).then(|| view! {
                <p class="field-hint">
                    {format!(
                        "Once Reelix works the way you want, you can stop {0}: on Windows, stop it and set its service to Disabled in services.msc (or close {0} and remove it from startup). Reelix doesn't change {0}.",
                        run.source.label()
                    )}
                </p>
            })}
            {groups
                .into_iter()
                .map(|(cat, items)| {
                    let summary = counts(&items);
                    // Only what needs attention is listed; the rest is summarised.
                    let notable: Vec<MigrateItem> = items
                        .into_iter()
                        .filter(|i| matches!(i.action, MigrateAction::Skip | MigrateAction::Failed) || i.detail.is_some())
                        .collect();
                    view! {
                        <details class="migrate-group">
                            <summary><strong>{cat.label()}</strong><span class="muted">{format!(" {summary}")}</span></summary>
                            {if notable.is_empty() {
                                ().into_any()
                            } else {
                                view! {
                                    <DataTable caption=cat.label() headers=vec!["Name", "Result", "Notes"]>
                                        {notable
                                            .into_iter()
                                            .map(|i| view! {
                                                <tr>
                                                    <td><span class="cell-strong">{i.name.clone()}</span></td>
                                                    <td>{action_badge(i.action)}</td>
                                                    <td><span class="cell-sub">{i.detail.clone()}</span></td>
                                                </tr>
                                            })
                                            .collect_view()}
                                    </DataTable>
                                }
                                .into_any()
                            }}
                        </details>
                    }
                })
                .collect_view()}
        </Card>
    }
}
