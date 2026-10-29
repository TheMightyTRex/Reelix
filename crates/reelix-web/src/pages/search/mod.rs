//! `/search?q=` — combined TMDB search results with availability.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};
use reelix_api_types::requests::{DiscoverItemDto, PersonDto, SearchPageDto};

use crate::api;
use crate::components::media_card::{MediaCard, MediaCardSkeleton, MediaGrid};
use crate::components::request_status::{item_badge, kind_label, media_href, url_encode};
use crate::components::{EmptyState, ErrorBox, Icon};

/// Route of the search results page for `q`.
pub fn search_href(q: &str) -> String {
    format!("/search?q={}", url_encode(q.trim()))
}

#[component]
pub fn SearchPage() -> impl IntoView {
    let query = use_query_map();
    let navigate = use_navigate();
    let q = Memo::new(move |_| query.with(|m| m.get("q").unwrap_or_default().trim().to_string()));
    let input = RwSignal::new(String::new());
    let items = RwSignal::new(Vec::<DiscoverItemDto>::new());
    let people = RwSignal::new(Vec::<PersonDto>::new());
    let page = RwSignal::new(1u32);
    let total_pages = RwSignal::new(0u32);
    let total = RwSignal::new(0u32);
    let loading = RwSignal::new(false);
    let error = RwSignal::new(None::<api::ApiError>);

    let load = move |p: u32| {
        let text = q.get_untracked();
        if text.is_empty() {
            return;
        }
        loading.set(true);
        spawn_local(async move {
            let url = format!("/search?q={}&page={p}", url_encode(&text));
            match api::get::<SearchPageDto>(&url).await {
                Ok(res) if q.get_untracked() == text => {
                    page.set(p);
                    total_pages.set(res.total_pages);
                    total.set(res.total_results);
                    if p == 1 {
                        people.set(res.people);
                    }
                    items.update(|v| {
                        if p == 1 {
                            v.clear();
                        }
                        for it in res.results {
                            if !v.iter().any(|x| x.kind == it.kind && x.tmdb_id == it.tmdb_id) {
                                v.push(it);
                            }
                        }
                    });
                    error.set(None);
                }
                Ok(_) => {}
                Err(e) => error.set(Some(e)),
            }
            loading.set(false);
        });
    };

    Effect::new(move |_| {
        let text = q.get();
        input.set(text.clone());
        items.set(Vec::new());
        people.set(Vec::new());
        total.set(0);
        total_pages.set(0);
        error.set(None);
        if !text.is_empty() {
            load(1);
        }
    });

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let text = input.get_untracked();
        if !text.trim().is_empty() {
            navigate(&search_href(&text), Default::default());
        }
    };

    let summary = move || {
        let text = q.get();
        if text.is_empty() {
            return String::new();
        }
        if loading.get() && items.with(Vec::is_empty) {
            return format!("Searching for “{text}”…");
        }
        format!("{} results for “{text}”", total.get())
    };

    view! {
        <div class="page-header">
            <div>
                <h1 class="page-title">"Search"</h1>
                <p class="page-subtitle" role="status">{summary}</p>
            </div>
        </div>
        <form class="search-page-form" role="search" on:submit=submit>
            <label class="visually-hidden" for="search-page-input">"Search movies and series"</label>
            <input id="search-page-input" class="input" type="search" placeholder="Search movies, series, people…"
                autocomplete="off" prop:value=move || input.get() on:input=move |e| input.set(event_target_value(&e)) />
            <button type="submit" class="btn btn-primary">
                <Icon name="search" />
                "Search"
            </button>
        </form>
        {move || error.get().map(|e| view! { <ErrorBox error=e on_retry=Callback::new(move |()| load(page.get_untracked())) /> })}
        {move || {
            let ps = people.get();
            (!ps.is_empty()).then(|| view! {
                <div class="people-row">
                    <span class="field-label">"People"</span>
                    <div class="chip-list">
                        {ps.into_iter().map(|p| {
                            let dept = p.known_for_department.clone().unwrap_or_default();
                            view! { <span class="chip" title=dept>{p.name}</span> }
                        }).collect_view()}
                    </div>
                </div>
            })
        }}
        {move || {
            if q.get().is_empty() {
                return view! {
                    <EmptyState icon="search" title="Search for something to watch"
                        description="Find movies and series on TMDB and see at a glance what's available or already requested." />
                }.into_any();
            }
            if !loading.get() && error.get().is_none() && items.with(Vec::is_empty) {
                return view! {
                    <EmptyState icon="search" title="No results" description="Try a different spelling or the original title." />
                }.into_any();
            }
            view! {
                <MediaGrid>
                    {move || items.get().into_iter().map(|item| view! {
                        <MediaCard
                            href=media_href(item.kind, item.tmdb_id)
                            title=item.title.clone()
                            subtitle=item.year.map(|y| y.to_string())
                            poster=item.poster.clone()
                            kind_label=kind_label(item.kind).to_string()
                            badge=item_badge(&item)
                        />
                    }).collect_view()}
                    {move || (loading.get() && items.with(Vec::is_empty)).then(|| view! { <MediaCardSkeleton count=12 /> })}
                </MediaGrid>
            }.into_any()
        }}
        <div class="load-more">
            <Show when=move || page.get() < total_pages.get() && !items.with(Vec::is_empty)>
                <button type="button" class="btn btn-secondary" disabled=move || loading.get()
                    on:click=move |_| load(page.get_untracked() + 1)>
                    {move || if loading.get() { "Loading…" } else { "Load more" }}
                </button>
            </Show>
        </div>
    }
}
