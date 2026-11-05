//! `/radio` — the followed radio shows and podcasts: artwork grid with station / followed /
//! monitored filters, paging and an A–Z jump list from the sort title.

use std::time::Duration;

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_query_map;
use reelix_api_types::radio::{Paged, RadioShowDto, RadioStationDto};

use crate::api;
use crate::components::library_common::{Pager, on_entity_changed, pref_get, pref_set};
use crate::components::library_forms::{require_library, with_query};
use crate::components::radio::{RADIO_ENTITIES, RadioShowCard, letter_of, search_all_missing};
use crate::components::{EmptyState, ErrorBox, Icon, PageHeader, Skeleton};

const PAGE_SIZE: u32 = 48;
const LETTERS: &str = "#ABCDEFGHIJKLMNOPQRSTUVWXYZ";

fn letter_anchor(c: char) -> String {
    if c == '#' {
        "radio-letter-num".into()
    } else {
        format!("radio-letter-{c}")
    }
}

#[component]
pub fn RadioShowsPage() -> impl IntoView {
    require_library();
    let query = use_query_map();
    let initial_station = query.with_untracked(|q| q.get("station")).unwrap_or_default();
    let q = RwSignal::new(String::new());
    let station = RwSignal::new(initial_station);
    let followed = RwSignal::new(pref_get("radio.followed").unwrap_or_else(|| "all".into()));
    let monitored = RwSignal::new(pref_get("radio.monitored").unwrap_or_else(|| "all".into()));
    let page = RwSignal::new(1u32);

    Effect::new(move |_| {
        pref_set("radio.followed", &followed.get());
        pref_set("radio.monitored", &monitored.get());
    });
    // A new filter starts from the first page.
    Effect::new(move |_| {
        let _ = (q.get(), station.get(), followed.get(), monitored.get());
        page.set(1);
    });

    let stations = LocalResource::new(|| api::get::<Vec<RadioStationDto>>("/radio/stations"));
    // Every matching show, sorted by sort title; paged here so the A–Z list can reach any letter.
    let shows = LocalResource::new(move || {
        let mut p: Vec<(&str, String)> = vec![
            ("search", q.get().trim().to_string()),
            ("station_id", station.get()),
            ("sort", "sort_title".into()),
            ("dir", "asc".into()),
        ];
        match followed.get().as_str() {
            "yes" => p.push(("followed", "true".into())),
            "no" => p.push(("followed", "false".into())),
            _ => {}
        }
        match monitored.get().as_str() {
            "yes" => p.push(("monitored", "true".into())),
            "no" => p.push(("monitored", "false".into())),
            _ => {}
        }
        let url = with_query("/radio/shows", &p);
        async move { api::get::<Paged<RadioShowDto>>(&url).await }
    });
    on_entity_changed(RADIO_ENTITIES, true, move || {
        shows.refetch();
        stations.refetch();
    });

    let items = Memo::new(move |_| {
        shows
            .get()
            .and_then(Result::ok)
            .map(|p| p.items)
            .unwrap_or_default()
    });
    let total = Signal::derive(move || items.with(|i| i.len() as u64));

    let jump = move |c: char| {
        let Some(idx) = items.with(|list| list.iter().position(|s| letter_of(&s.sort_title) == c)) else {
            return;
        };
        page.set(idx as u32 / PAGE_SIZE + 1);
        let id = letter_anchor(c);
        set_timeout(
            move || {
                if let Some(el) = document().get_element_by_id(&id) {
                    el.scroll_into_view();
                    if let Ok(link) = wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(
                        el.query_selector("a").ok().flatten().unwrap_or(el),
                    ) {
                        let _ = link.focus();
                    }
                }
            },
            Duration::from_millis(30),
        );
    };

    let jump_list = move || {
        let present: Vec<char> = items.with(|list| list.iter().map(|s| letter_of(&s.sort_title)).collect());
        (!present.is_empty()).then(|| {
            view! {
                <nav class="az-jump" aria-label="Jump to letter">
                    {LETTERS.chars().map(|c| {
                        let has = present.contains(&c);
                        view! {
                            <button type="button" class="az-btn" disabled=!has
                                aria-label=if c == '#' { "Jump to titles starting with a number or symbol".to_string() } else { format!("Jump to {c}") }
                                on:click=move |_| jump(c)>
                                {c.to_string()}
                            </button>
                        }
                    }).collect_view()}
                </nav>
            }
        })
    };

    let content = move || {
        match shows.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| shows.refetch()) /> }
                    .into_any();
            }
            Some(Ok(_)) => {}
        }
        let list = items.get();
        if list.is_empty() {
            let filtered = !q.get_untracked().trim().is_empty()
                || !station.get_untracked().is_empty()
                || followed.get_untracked() != "all"
                || monitored.get_untracked() != "all";
            return if filtered {
                view! {
                    <div class="card">
                        <EmptyState icon="search" title="No shows match" description="Try clearing the search or filters." />
                    </div>
                }
                .into_any()
            } else {
                view! {
                    <div class="card">
                        <EmptyState icon="radio" title="No radio shows yet"
                            description="Follow a radio programme or podcast: search by name or paste a show page or feed link.">
                            <A href="/radio/add" attr:class="btn btn-primary"><Icon name="plus" />"Add show"</A>
                            <A href="/radio/review" attr:class="btn btn-secondary"><Icon name="folder" />"Review library"</A>
                        </EmptyState>
                    </div>
                }
                .into_any()
            };
        }
        let start = (page.get().saturating_sub(1) * PAGE_SIZE) as usize;
        // The first show of each letter on this page carries the jump anchor.
        let mut prev = start
            .checked_sub(1)
            .and_then(|i| list.get(i))
            .map(|s| letter_of(&s.sort_title));
        let cards = list
            .into_iter()
            .skip(start)
            .take(PAGE_SIZE as usize)
            .map(|s| {
                let c = letter_of(&s.sort_title);
                let anchor = (prev != Some(c)).then(|| letter_anchor(c));
                prev = Some(c);
                view! { <RadioShowCard show=s anchor=anchor /> }
            })
            .collect_view();
        view! {
            <div class="poster-grid radio-grid">{cards}</div>
            <Pager page=page page_size=PAGE_SIZE total=total />
        }
        .into_any()
    };

    let count_label = move || {
        shows
            .get()
            .and_then(Result::ok)
            .map(|p| format!("{} show(s)", p.total))
            .unwrap_or_default()
    };

    view! {
        <PageHeader title="Radio">
            <button type="button" class="btn btn-secondary" on:click=move |_| search_all_missing()>
                <Icon name="search" />
                "Search all missing"
            </button>
            <A href="/radio/add" attr:class="btn btn-primary"><Icon name="plus" />"Add show"</A>
        </PageHeader>
        <div class="lib-toolbar">
            <input class="input grow" type="search" placeholder="Filter by title…" aria-label="Filter by title"
                prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e)) />
            <select class="input" aria-label="Station" on:change=move |e| station.set(event_target_value(&e))>
                <option value="" selected=move || station.get().is_empty()>"All stations"</option>
                {move || stations.get().and_then(Result::ok).unwrap_or_default().into_iter().map(|st| {
                    let v = st.id.to_string();
                    let v2 = v.clone();
                    view! { <option value=v selected=move || station.get() == v2>{st.name}</option> }
                }).collect_view()}
            </select>
            <select class="input" aria-label="Followed" on:change=move |e| followed.set(event_target_value(&e))>
                {[("all", "Followed or not"), ("yes", "Followed"), ("no", "Not followed")]
                    .into_iter()
                    .map(|(v, l)| view! { <option value=v selected=move || followed.get() == v>{l}</option> })
                    .collect_view()}
            </select>
            <select class="input" aria-label="Monitored" on:change=move |e| monitored.set(event_target_value(&e))>
                {[("all", "Monitored or not"), ("yes", "Monitored"), ("no", "Unmonitored")]
                    .into_iter()
                    .map(|(v, l)| view! { <option value=v selected=move || monitored.get() == v>{l}</option> })
                    .collect_view()}
            </select>
        </div>
        <p class="lib-count" aria-live="polite">{count_label}</p>
        {jump_list}
        {content}
    }
}
