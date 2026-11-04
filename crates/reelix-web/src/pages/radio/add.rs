//! `/radio/add` — find a radio show or podcast by name (one source or all that can search) or
//! paste a show page / feed link, preview it, choose how to follow it and add it.

use std::time::Duration;

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_query_map};
use reelix_api_types::radio::{
    AddRadioShowRequest, RadioProviderDto, RadioShowDto, RadioShowHitDto, RadioShowPreviewDto,
};

use crate::api;
use crate::components::library_common::{Segmented, pref_get, pref_set};
use crate::components::library_forms::{enc, opt_from, require_library};
use crate::components::radio::{
    FOLLOW, RadioArtwork, RadioOptions, RadioProfileSelect, RadioRootSelect, RadioTagPicker, length, opt_id,
};
use crate::components::{Card, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, Spinner};
use crate::format::http_url;
use crate::state::Toasts;

/// What is about to be added: a search hit or a pasted link (with its preview).
#[derive(Clone)]
enum Target {
    Hit(RadioShowHitDto),
    Link(String, RadioShowPreviewDto),
}

impl Target {
    fn hit(&self) -> &RadioShowHitDto {
        match self {
            Self::Hit(h) => h,
            Self::Link(_, p) => &p.show,
        }
    }
}

#[component]
pub fn AddRadioShowPage() -> impl IntoView {
    require_library();
    let query = use_query_map();
    let initial = query.with_untracked(|q| q.get("term").unwrap_or_default());
    let mode = RwSignal::new(
        if initial.starts_with("http") {
            "link"
        } else {
            "search"
        }
        .to_string(),
    );
    let input = RwSignal::new(initial.clone());
    let term = RwSignal::new(initial);
    let provider = RwSignal::new(pref_get("radio.add.provider").unwrap_or_default());
    let target = RwSignal::new(None::<Target>);

    let providers = LocalResource::new(|| api::get::<Vec<RadioProviderDto>>("/radio/providers"));
    let searchable = move || {
        providers
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.supports_search && p.enabled)
            .collect::<Vec<_>>()
    };

    let pending = StoredValue::new(0u32);
    let on_input = move |e: ev::Event| {
        let v = event_target_value(&e);
        input.set(v.clone());
        if mode.get_untracked() != "search" {
            return;
        }
        let n = pending.get_value() + 1;
        pending.set_value(n);
        // Sources are gently paced: wait for a pause in typing.
        set_timeout(
            move || {
                if pending.try_get_value() == Some(n) {
                    let _ = term.try_set(v.trim().to_string());
                }
            },
            Duration::from_millis(600),
        );
    };
    let submit = move || term.set(input.get_untracked().trim().to_string());

    // Switching between search and link starts over (a name is not a link).
    Effect::new(move |prev: Option<()>| {
        mode.track();
        if prev.is_some() {
            target.set(None);
            term.set(String::new());
        }
    });

    let results = LocalResource::new(move || {
        let t = term.get();
        let m = mode.get();
        let p = provider.get();
        async move {
            if t.trim().len() < 2 {
                return None;
            }
            Some(if m == "link" {
                api::get::<RadioShowPreviewDto>(&format!("/radio/preview?link={}", enc(&t)))
                    .await
                    .map(|pv| vec![Target::Link(t.clone(), pv)])
            } else {
                let mut url = format!("/radio/search?q={}", enc(&t));
                if !p.is_empty() {
                    url.push_str(&format!("&provider={}", enc(&p)));
                }
                api::get::<Vec<RadioShowHitDto>>(&url)
                    .await
                    .map(|hits| hits.into_iter().map(Target::Hit).collect())
            })
        }
    });
    // A pasted link shows its preview straight away.
    Effect::new(move |_| {
        if let Some(Some(Ok(list))) = results.get()
            && let [t @ Target::Link(..)] = list.as_slice()
        {
            target.set(Some(t.clone()));
        }
    });

    let list = move || {
        match results.get() {
        None => view! { <Skeleton lines=4 /> }.into_any(),
        Some(None) => view! {
            <div class="card">
                {move || if mode.get() == "link" {
                    view! { <EmptyState icon="radio" title="Paste a link" description="A show page (for example a BBC Sounds programme) or a podcast feed URL." /> }.into_any()
                } else {
                    view! { <EmptyState icon="search" title="Find a radio show or podcast" description="Search the sources that support it, or paste a link instead." /> }.into_any()
                }}
            </div>
        }
        .into_any(),
        Some(Some(Err(e))) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| results.refetch()) /> }.into_any(),
        Some(Some(Ok(list))) if list.is_empty() => view! {
            <div class="card">
                <EmptyState icon="search" title="No matches" description="Try a different spelling, another source, or paste the show's link." />
            </div>
        }
        .into_any(),
        Some(Some(Ok(list))) => {
            if let [Target::Link(..)] = list.as_slice() {
                return ().into_any();
            }
            view! {
                <div class="lookup-list">
                    {list.into_iter().map(|t| {
                        let h = t.hit().clone();
                        let action = match h.show_id {
                            Some(id) => view! {
                                <A href=format!("/radio/shows/{id}") attr:class="btn btn-sm btn-secondary"><Icon name="check" />"In library"</A>
                            }.into_any(),
                            None => {
                                let t2 = t.clone();
                                view! {
                                    <button type="button" class="btn btn-sm btn-primary" on:click=move |_| {
                                        target.set(Some(t2.clone()));
                                        focus_add_form();
                                    }>
                                        <Icon name="plus" />
                                        "Add"
                                    </button>
                                }.into_any()
                            }
                        };
                        view! {
                            <article class="lookup-item">
                                <RadioArtwork title=h.title.clone() url={None::<String>} />
                                <HitSummary hit=h />
                                <div>{action}</div>
                            </article>
                        }
                    }).collect_view()}
                </div>
            }
            .into_any()
        }
    }
    };

    view! {
        <PageHeader title="Add radio show" subtitle="Follow a radio programme or podcast; its episodes are matched against your Audiobookshelf podcast library." />
        <div class="lib-toolbar">
            <Segmented value=mode options=vec![("search", "Search by name"), ("link", "Paste a link")] label="How to find the show" />
            <form class="search add-search grow" on:submit=move |e: ev::SubmitEvent| { e.prevent_default(); submit(); }>
                <label for="radio-add-term" class="visually-hidden">{move || if mode.get() == "link" { "Link" } else { "Show name" }}</label>
                <input id="radio-add-term" class="input grow" autofocus
                    type=move || if mode.get() == "link" { "url" } else { "search" }
                    placeholder=move || if mode.get() == "link" { "https://www.bbc.co.uk/programmes/… or a feed URL" } else { "Show or podcast name" }
                    prop:value=move || input.get() on:input=on_input />
            </form>
            <Show when=move || mode.get() == "search">
                <select class="input" aria-label="Source" on:change=move |e| {
                    let v = event_target_value(&e);
                    pref_set("radio.add.provider", &v);
                    provider.set(v);
                }>
                    <option value="" selected=move || provider.get().is_empty()>"All sources"</option>
                    {move || searchable().into_iter().map(|p| {
                        let v = p.id.clone();
                        let v2 = v.clone();
                        view! { <option value=v selected=move || provider.get() == v2>{p.name}</option> }
                    }).collect_view()}
                </select>
            </Show>
            <Show when=move || mode.get() == "link">
                <button type="button" class="btn btn-primary" on:click=move |_| submit()>
                    <Icon name="search" />
                    "Preview"
                </button>
            </Show>
        </div>
        {move || {
            let none_can_search = providers.get().and_then(Result::ok).is_some_and(|_| searchable().is_empty());
            (none_can_search && mode.get() == "search").then(|| view! {
                <p class="field-hint">"No enabled source can search by name yet — paste a show page or feed link instead."</p>
            })
        }}
        {move || target.get().map(|t| view! { <AddForm target=t on_cancel=Callback::new(move |()| target.set(None)) /> })}
        {list}
    }
}

fn focus_add_form() {
    set_timeout(
        || {
            if let Some(el) = document().get_element_by_id("radio-add-form") {
                el.scroll_into_view();
            }
            if let Some(el) = document()
                .get_element_by_id("radio-add-follow")
                .and_then(|e| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(e).ok())
            {
                let _ = el.focus();
            }
        },
        Duration::from_millis(30),
    );
}

/// Title, station, author, episode count and synopsis of a hit.
#[component]
fn HitSummary(hit: RadioShowHitDto) -> impl IntoView {
    let web = hit.web_url.as_deref().and_then(http_url);
    view! {
        <div class="overview-body">
            <span class="overview-title">{hit.title.clone()}</span>
            <div class="meta-line">
                {hit.station_name.clone().map(|s| view! { <span>{s}</span> })}
                {hit.author.clone().map(|a| view! { <span>{a}</span> })}
                {hit.episode_count.map(|n| view! { <span>{format!("{n} episode(s)")}</span> })}
                <span class="badge tone-neutral">{hit.provider.clone()}</span>
                {web.map(|u| view! { <a href=u target="_blank" rel="noopener noreferrer">"Show page"<Icon name="external" /></a> })}
            </div>
            {hit.synopsis.clone().map(|s| view! { <p class="overview-text">{s}</p> })}
        </div>
    }
}

/// Preview card plus the follow / monitored / root folder / profile / tags choices.
#[component]
fn AddForm(target: Target, on_cancel: Callback<()>) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let opts = RadioOptions::load();
    let follow = RwSignal::new(pref_get("radio.add.follow").unwrap_or_else(|| "default".into()));
    let monitored = RwSignal::new(true);
    let root = RwSignal::new(
        pref_get("radio.add.root")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0i64),
    );
    let profile = RwSignal::new(
        pref_get("radio.add.profile")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0i64),
    );
    let tags = RwSignal::new(Vec::<i64>::new());
    let busy = RwSignal::new(false);
    let hit = target.hit().clone();
    let already = hit.show_id;
    let target = StoredValue::new(target);

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let f = follow.get_untracked();
        let (r, p) = (root.get_untracked(), profile.get_untracked());
        pref_set("radio.add.follow", &f);
        pref_set("radio.add.root", &r.to_string());
        pref_set("radio.add.profile", &p.to_string());
        let mut req = AddRadioShowRequest {
            follow: opt_from(&FOLLOW, &f),
            monitored: monitored.get_untracked(),
            root_folder_id: opt_id(r),
            quality_profile_id: opt_id(p),
            tags: tags.get_untracked(),
            ..Default::default()
        };
        match target.get_value() {
            Target::Hit(h) => {
                req.provider = Some(h.provider);
                req.external_id = Some(h.external_id);
            }
            Target::Link(link, _) => req.link = Some(link),
        }
        busy.set(true);
        let navigate = navigate.clone();
        spawn_local(async move {
            let res = api::post::<_, RadioShowDto>("/radio/shows", &req).await;
            busy.set(false);
            match res {
                Ok(s) => {
                    toasts.success(format!("Added “{}”.", s.title));
                    navigate(&format!("/radio/shows/{}", s.id), Default::default());
                }
                Err(e) => toasts.error(format!("Could not add: {e}")),
            }
        });
    };

    let preview = target.with_value(|t| match t {
        Target::Link(_, p) => Some(p.clone()),
        Target::Hit(_) => None,
    });

    view! {
        <div id="radio-add-form">
            <Card title="Add this show" class="radio-add-card">
                <div class="radio-preview">
                    <RadioArtwork title=hit.title.clone() url={None::<String>} />
                    <div class="stack">
                        <HitSummary hit=hit.clone() />
                        {preview.map(|p| view! {
                            <div class="meta-line">
                                {p.first_broadcast.map(|d| view! { <span>{format!("First broadcast {}", crate::format::date(d))}</span> })}
                                <span>{format!("{} series", p.series_count)}</span>
                                <span>{format!("{} episode(s)", p.episode_count)}</span>
                            </div>
                            {(!p.latest_episodes.is_empty()).then(|| view! {
                                <div class="table-wrap">
                                    <table class="table">
                                        <caption class="visually-hidden">"Latest episodes"</caption>
                                        <thead><tr>
                                            <th scope="col">"#"</th>
                                            <th scope="col">"Latest episodes"</th>
                                            <th scope="col">"First broadcast"</th>
                                            <th scope="col">"Length"</th>
                                        </tr></thead>
                                        <tbody>
                                            {p.latest_episodes.into_iter().map(|e| view! {
                                                <tr>
                                                    <td class="mono nowrap">{match (e.series_number, e.number) {
                                                        (Some(s), Some(n)) => format!("S{s:02}E{n:02}"),
                                                        (None, Some(n)) => format!("E{n:02}"),
                                                        _ => "—".into(),
                                                    }}</td>
                                                    <td>{e.title}</td>
                                                    <td class="nowrap">{e.first_broadcast.map(crate::format::date).unwrap_or_else(|| "—".into())}</td>
                                                    <td class="nowrap">{length(e.duration_secs)}</td>
                                                </tr>
                                            }).collect_view()}
                                        </tbody>
                                    </table>
                                </div>
                            })}
                        })}
                    </div>
                </div>
                {match already {
                    Some(id) => view! {
                        <p class="field-hint">"This show is already in your library. "
                            <A href=format!("/radio/shows/{id}")>"Open it"</A>
                        </p>
                    }.into_any(),
                    None => view! {
                        <form class="form" on:submit=submit>
                            <div class="field-row">
                                <div class="field">
                                    <label for="radio-add-follow">"Follow"</label>
                                    <select id="radio-add-follow" class="input" on:change=move |e| follow.set(event_target_value(&e))>
                                        <option value="default" selected=move || follow.get() == "default">"Default from the radio settings"</option>
                                        {FOLLOW.iter().map(|(_, k, l)| {
                                            let k: &'static str = k;
                                            view! { <option value=k selected=move || follow.get() == k>{*l}</option> }
                                        }).collect_view()}
                                    </select>
                                </div>
                                <div class="field">
                                    <label for="radio-add-root">"Root folder"</label>
                                    <RadioRootSelect opts=opts value=root id="radio-add-root" />
                                </div>
                            </div>
                            <div class="field-row">
                                <div class="field">
                                    <label for="radio-add-profile">"Quality profile"</label>
                                    <RadioProfileSelect opts=opts value=profile id="radio-add-profile" />
                                    <span class="field-hint">"An audiobook profile, used when episodes are searched for on indexers."</span>
                                </div>
                                <div class="field field-check">
                                    <label class="checkbox">
                                        <input type="checkbox" prop:checked=move || monitored.get() on:change=move |e| monitored.set(event_target_checked(&e)) />
                                        "Monitored"
                                    </label>
                                </div>
                            </div>
                            <div class="field">
                                <span class="field-label">"Tags"</span>
                                <RadioTagPicker opts=opts value=tags />
                            </div>
                            <div class="modal-footer">
                                <button type="button" class="btn btn-ghost" on:click=move |_| on_cancel.run(())>"Cancel"</button>
                                <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                                    {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="plus" /> }.into_any() }}
                                    "Add show"
                                </button>
                            </div>
                        </form>
                    }.into_any(),
                }}
            </Card>
        </div>
    }
}
