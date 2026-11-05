//! `/indexers/search` — manual search across indexers; grab a release straight to a client.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::pipeline::{GrabRequest, GrabResponse};
use reelix_api_types::providers::{CategoryDto, IndexerSearchResponse, Protocol, ProviderDto, ReleaseDto};

use crate::api;
use crate::components::library_common::bytes;
use crate::components::{
    Card, CopyButton, DataTable, EmptyState, Icon, PageHeader, Spinner, StatusBadge, Tone,
};
use crate::state::Toasts;

fn age(r: &ReleaseDto) -> String {
    match r.publish_date {
        None => "—".into(),
        Some(d) => {
            let h = (chrono::Utc::now() - d).num_hours().max(0);
            if h < 48 {
                format!("{h} h")
            } else {
                format!("{} d", h / 24)
            }
        }
    }
}

#[component]
pub fn IndexerSearchPage() -> impl IntoView {
    let toasts = Toasts::expect();
    let query = RwSignal::new(String::new());
    let kind = RwSignal::new("basic".to_owned());
    let cats = RwSignal::new(Vec::<u32>::new());
    let picked = RwSignal::new(Vec::<i64>::new());
    let searching = RwSignal::new(false);
    let results = RwSignal::new(None::<IndexerSearchResponse>);
    let sort = RwSignal::new("age".to_owned());
    let categories = LocalResource::new(|| api::get::<Vec<CategoryDto>>("/indexers/categories"));
    let indexers = LocalResource::new(|| api::get::<Vec<ProviderDto>>("/indexers"));
    let grabbing = RwSignal::new(None::<String>);
    let grabbed = RwSignal::new(Vec::<String>::new());
    // Raw grab: no media mapping, so the download is tracked until imported manually.
    let grab = move |r: ReleaseDto| {
        let key = format!("{}|{}", r.indexer_id, r.guid);
        let req = GrabRequest {
            guid: r.guid.clone(),
            indexer_id: r.indexer_id,
            force: false,
            movie_id: None,
            episode_ids: None,
            download_client_id: None,
            download_url: r.download_url.clone().or(r.magnet_url.clone()),
            title: Some(r.title.clone()),
        };
        grabbing.set(Some(key.clone()));
        spawn_local(async move {
            match api::post::<_, GrabResponse>("/releases/grab", &req).await {
                Ok(resp) => {
                    toasts.success(format!("Sent “{}” to {}.", r.title, resp.download_client));
                    grabbed.update(|g| g.push(key));
                }
                Err(e) => toasts.error(format!("Grab failed: {e}")),
            }
            grabbing.set(None);
        });
    };

    let run = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let q = query.get_untracked();
        let mut url = format!(
            "/indexers/search?kind={}&q={}",
            kind.get_untracked(),
            js_encode(&q)
        );
        let c = cats.get_untracked();
        if !c.is_empty() {
            url.push_str(&format!(
                "&cat={}",
                c.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
            ));
        }
        let ids = picked.get_untracked();
        if !ids.is_empty() {
            url.push_str(&format!(
                "&ids={}",
                ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
            ));
        }
        searching.set(true);
        spawn_local(async move {
            match api::get::<IndexerSearchResponse>(&url).await {
                Ok(r) => results.set(Some(r)),
                Err(e) => toasts.error(format!("Search failed: {e}")),
            }
            searching.set(false);
        });
    };

    let sorted = move || {
        results.get().map(|r| {
            let mut rel = r.releases.clone();
            match sort.get().as_str() {
                "size" => rel.sort_by_key(|a| std::cmp::Reverse(a.size)),
                "peers" => rel.sort_by_key(|a| std::cmp::Reverse(a.seeders)),
                "title" => rel.sort_by_key(|a| a.title.to_lowercase()),
                _ => rel.sort_by_key(|a| std::cmp::Reverse(a.publish_date)),
            }
            (rel, r.indexers)
        })
    };

    view! {
        <PageHeader title="Indexer search" subtitle="Search every indexer at once and grab straight to a download client. Grabs from here aren't linked to library media; import them from the queue." />
        <Card>
            <form class="form" on:submit=run>
                <div class="field-row">
                    <div class="field grow">
                        <label for="is-q">"Query"</label>
                        <input id="is-q" class="input" type="search" autofocus placeholder="Title, S01E01, …"
                            prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="is-kind">"Type"</label>
                        <select id="is-kind" class="input" on:change=move |e| kind.set(event_target_value(&e))>
                            <option value="basic">"Any"</option>
                            <option value="movie">"Movie"</option>
                            <option value="tv">"TV"</option>
                        </select>
                    </div>
                </div>
                <details class="field">
                    <summary>"Categories & indexers"</summary>
                    <div class="multi-select" role="group" aria-label="Categories">
                        {move || categories.get().and_then(Result::ok).unwrap_or_default().into_iter().map(|c| {
                            let id = c.id;
                            view! {
                                <label class="checkbox">
                                    <input type="checkbox" prop:checked=move || cats.with(|v| v.contains(&id))
                                        on:change=move |e| { let on = event_target_checked(&e); cats.update(|v| { v.retain(|x| *x != id); if on { v.push(id) } }); } />
                                    {c.name}
                                </label>
                            }
                        }).collect_view()}
                    </div>
                    <div class="multi-select" role="group" aria-label="Indexers">
                        {move || indexers.get().and_then(Result::ok).unwrap_or_default().into_iter().filter(|i| i.enabled).map(|i| {
                            let id = i.id;
                            view! {
                                <label class="checkbox">
                                    <input type="checkbox" prop:checked=move || picked.with(|v| v.contains(&id))
                                        on:change=move |e| { let on = event_target_checked(&e); picked.update(|v| { v.retain(|x| *x != id); if on { v.push(id) } }); } />
                                    {i.name}
                                </label>
                            }
                        }).collect_view()}
                    </div>
                    <p class="field-hint">"Nothing selected = defaults (all categories / every indexer with interactive search on)."</p>
                </details>
                <div class="form-actions">
                    <button type="submit" class="btn btn-primary" disabled=move || searching.get()>"Search"</button>
                </div>
            </form>
        </Card>
        {move || searching.get().then(|| view! { <Spinner label="Searching indexers…" /> })}
        {move || sorted().map(|(rel, idx)| view! {
            <div class="chip-list search-outcomes">
                {idx.into_iter().map(|o| {
                    let (tone, label) = match &o.error {
                        Some(e) => (Tone::Danger, format!("{}: {e}", o.name)),
                        None => (Tone::Success, format!("{}: {} in {} ms", o.name, o.count, o.elapsed_ms)),
                    };
                    view! { <StatusBadge tone=tone label=label /> }
                }).collect_view()}
            </div>
            <div class="toolbar">
                <label for="is-sort" class="field-label">"Sort"</label>
                <select id="is-sort" class="input" on:change=move |e| sort.set(event_target_value(&e))>
                    <option value="age">"Newest"</option>
                    <option value="size">"Size"</option>
                    <option value="peers">"Seeders"</option>
                    <option value="title">"Title"</option>
                </select>
            </div>
            {if rel.is_empty() {
                view! { <EmptyState icon="search" title="No releases found" /> }.into_any()
            } else {
                view! {
                    <Card>
                        <DataTable caption="Releases" headers=vec!["Protocol", "Age", "Title", "Indexer", "Size", "Peers", "Flags", ""]>
                            {rel.into_iter().map(|r| {
                                let link = r.magnet_url.clone().or(r.download_url.clone()).unwrap_or_default();
                                let proto = r.protocol.as_str();
                                view! {
                                    <tr>
                                        <td><span class=format!("badge proto-{proto}")>{proto}</span></td>
                                        <td>{age(&r)}</td>
                                        <td class="release-title">
                                            {match r.info_url.as_deref().and_then(crate::format::http_url) {
                                                Some(u) => view! { <a href=u target="_blank" rel="noopener noreferrer">{r.title.clone()}</a> }.into_any(),
                                                None => view! { <span>{r.title.clone()}</span> }.into_any(),
                                            }}
                                        </td>
                                        <td>{r.indexer.clone()}</td>
                                        <td>{r.size.map(bytes).unwrap_or_else(|| "—".into())}</td>
                                        <td>{match (r.seeders, r.peers) {
                                            _ if r.protocol == Protocol::Soulseek => {
                                                let peer = crate::components::interactive_search::soulseek_peer(r.soulseek.as_ref());
                                                match &r.soulseek {
                                                    Some(s) => format!("@{} · {peer}", s.username),
                                                    None => peer,
                                                }
                                            }
                                            (Some(s), Some(p)) => format!("{s} / {p}"),
                                            (Some(s), None) => s.to_string(),
                                            _ => "—".into(),
                                        }}</td>
                                        <td><span class="chip-list">{r.indexer_flags.iter().map(|f| view! { <span class="chip">{f.replace('_', " ")}</span> }).collect_view()}</span></td>
                                        <td class="cell-actions">
                                            {(!link.is_empty()).then(|| {
                                                let key = format!("{}|{}", r.indexer_id, r.guid);
                                                let key2 = key.clone();
                                                let rel = StoredValue::new(r.clone());
                                                view! {
                                                    {move || if grabbed.with(|g| g.contains(&key)) {
                                                        view! { <span class="badge tone-success">"Sent"</span> }.into_any()
                                                    } else {
                                                        let busy = grabbing.with(|g| g.as_deref() == Some(key2.as_str()));
                                                        view! {
                                                            <button type="button" class="btn btn-sm btn-primary" disabled=move || grabbing.with(Option::is_some)
                                                                on:click=move |_| grab(rel.get_value())>
                                                                <Icon name="download" />
                                                                {if busy { "Grabbing…" } else { "Grab" }}
                                                            </button>
                                                        }.into_any()
                                                    }}
                                                    <CopyButton text=Signal::stored(link.clone()) label="Copy link" />
                                                }
                                            })}
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </DataTable>
                    </Card>
                }.into_any()
            }}
        })}
    }
}

/// Percent-encodes a query parameter value.
fn js_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
