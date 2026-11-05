//! Radio on Discover / Search: rows of the enabled radio sources (Apple's top podcasts,
//! Podcast Index trending, popular on BBC Sounds and on each BBC station), category chips
//! (under the source's headings, such as Comedy and Drama for the BBC), search results, and the
//! show dialog (synopsis, station, newest episodes) with Follow (MANAGE_LIBRARY: adds the
//! show directly) or Request (REQUEST_RADIO).

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::Permissions;
use reelix_api_types::radio::{AddRadioShowRequest, RadioShowDto};
use reelix_api_types::requests::{
    AvailabilityStatus, ContentDetailDto, ContentItemDto, ContentRequestDto, ContentRequestKind,
    ContentRowDto, CreateContentRequestDto, QuotaDto, RadioDiscoverDto, RadioDiscoverListDto,
    RadioSearchPageDto, RequestStatus, split_radio_foreign_id,
};

use super::content::CARD_BUTTON_STYLE;
use crate::api;
use crate::components::media_card::{BadgeKind, MediaBadge, MediaCardSkeleton, MediaGrid, MediaRow};
use crate::components::radio::{hit_artwork, length};
use crate::components::request_modal::quota_line;
use crate::components::request_status::{RequestStatusBadge, UserChip, refetch_on_change, url_encode};
use crate::components::{EmptyState, ErrorBox, Icon, Modal};
use crate::format;
use crate::state::{AuthState, Toasts};

/// Live changes that can change a card's follow / request state.
const ENTITIES: &[&str] = &["request", "radio_show"];

/// Whether the current user may follow shows directly (instead of requesting them).
fn can_follow(auth: &AuthState) -> bool {
    auth.has(Permissions::MANAGE_LIBRARY)
}

fn followed(item: &ContentItemDto) -> bool {
    item.availability == AvailabilityStatus::Available || item.library_id.is_some()
}

/// `/media/radio/{provider}/{external id}` of a radio item.
fn detail_path(foreign_id: &str) -> Option<String> {
    let (p, e) = split_radio_foreign_id(foreign_id)?;
    Some(format!("/media/radio/{}/{}", url_encode(p), url_encode(e)))
}

/// Corner badge: Following, else the visible request.
fn badge(item: &ContentItemDto) -> Option<(BadgeKind, bool)> {
    if followed(item) {
        return Some((BadgeKind::Available, true));
    }
    match item.request.as_ref().map(|r| r.status)? {
        RequestStatus::Pending | RequestStatus::Approved | RequestStatus::Processing => {
            Some((BadgeKind::Requested, false))
        }
        RequestStatus::Available | RequestStatus::PartiallyAvailable => Some((BadgeKind::Available, true)),
        RequestStatus::Declined | RequestStatus::Failed => None,
    }
}

/// Square artwork letterboxed in the cover shape; a radio icon when there is none or its host
/// is not allowed by the page.
#[component]
fn HitArt(#[prop(into)] url: Option<String>, #[prop(optional)] children: Option<Children>) -> impl IntoView {
    let failed = RwSignal::new(false);
    let src = hit_artwork(url.as_deref());
    view! {
        <div class="media-poster radio-art">
            {move || match src.clone().filter(|_| !failed.get()) {
                Some(src) => view! {
                    <img src=src alt="" loading="lazy" decoding="async" on:error=move |_| failed.set(true) />
                }
                .into_any(),
                None => view! {
                    <span class="media-poster-fallback radio-art-fallback" aria-hidden="true"><Icon name="radio" /></span>
                }
                .into_any(),
            }}
            {children.map(|c| c())}
        </div>
    }
}

/// One radio show tile.
#[component]
fn RadioCard(item: ContentItemDto, on_open: Callback<ContentItemDto>) -> impl IntoView {
    let sub = item.subtitle.clone().unwrap_or_default();
    let label = if sub.is_empty() {
        item.title.clone()
    } else {
        format!("{} — {sub}", item.title)
    };
    let mark = badge(&item);
    let title = item.title.clone();
    let cover = item.cover.clone();
    let stored = StoredValue::new(item);
    view! {
        <button type="button" class="media-card" style=CARD_BUTTON_STYLE aria-label=label
            on:click=move |_| on_open.run(stored.get_value())>
            <HitArt url=cover>
                <span class="media-kind"><Icon name="radio" />"Radio"</span>
                {mark.map(|(kind, following)| if following {
                    view! {
                        <span class=kind.class() title="Following">
                            <span aria-hidden="true">{kind.glyph()}</span>
                            <span class="media-badge-label">"Following"</span>
                        </span>
                    }.into_any()
                } else {
                    view! { <MediaBadge kind=kind /> }.into_any()
                })}
            </HitArt>
            <div class="media-meta">
                <span class="media-title">{title}</span>
                {(!sub.is_empty()).then(|| view! { <span class="media-sub">{sub.clone()}</span> })}
            </div>
        </button>
    }
}

fn cards(items: Vec<ContentItemDto>, on_open: Callback<ContentItemDto>) -> impl IntoView {
    items
        .into_iter()
        .map(|item| view! { <RadioCard item=item on_open=on_open /> })
        .collect_view()
}

/// Radio search results for `query`, with the show dialog.
#[component]
pub fn RadioResults(#[prop(into)] query: Signal<String>) -> impl IntoView {
    let open = RwSignal::new(false);
    let current = RwSignal::new(None::<ContentItemDto>);
    let res = LocalResource::new(move || {
        let q = query.get().trim().to_string();
        async move {
            if q.is_empty() {
                return Ok(None);
            }
            api::get::<RadioSearchPageDto>(&format!("/search/radio?q={}", url_encode(&q)))
                .await
                .map(|p| Some(p.results))
        }
    });
    refetch_on_change(ENTITIES, move || res.refetch());
    let on_open = Callback::new(move |item: ContentItemDto| {
        current.set(Some(item));
        open.set(true);
    });
    view! {
        {move || match res.get() {
            None => view! { <MediaGrid><MediaCardSkeleton count=12 /></MediaGrid> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| res.refetch()) /> }.into_any(),
            Some(Ok(None)) => view! {
                <EmptyState icon="search" title="Search for radio shows"
                    description="Find radio programmes and podcasts by name, or paste a link to a show page or podcast feed." />
            }.into_any(),
            Some(Ok(Some(items))) if items.is_empty() => view! {
                <EmptyState icon="search" title="No results" description="Try a different spelling, or paste the show's link." />
            }.into_any(),
            Some(Ok(Some(items))) => view! { <MediaGrid>{cards(items, on_open)}</MediaGrid> }.into_any(),
        }}
        <RadioShowModal open=open item=current on_changed=Callback::new(move |()| res.refetch()) />
    }
}

/// One radio Discover list as a scrolling row (`grid` = a full grid instead).
#[component]
fn RadioList(
    list: RadioDiscoverListDto,
    on_open: Callback<ContentItemDto>,
    #[prop(optional)] grid: bool,
) -> impl IntoView {
    let id = list.id.clone();
    let res = LocalResource::new(move || {
        let id = id.clone();
        async move { api::get::<ContentRowDto>(&format!("/discover/content/radio/{}", url_encode(&id))).await }
    });
    refetch_on_change(ENTITIES, move || res.refetch());
    let body = move || match res.get() {
        None => view! { <MediaCardSkeleton /> }.into_any(),
        Some(Err(e)) => view! { <p class="muted row-error">{e.message}</p> }.into_any(),
        Some(Ok(page)) if page.results.is_empty() => {
            view! { <p class="muted row-error">"Nothing here right now."</p> }.into_any()
        }
        Some(Ok(page)) => cards(page.results, on_open).into_any(),
    };
    let title = format!("{} · {}", list.name, list.provider_name);
    if grid {
        view! {
            <section class="media-row">
                <header class="media-row-head"><h2 class="media-row-title">{title}</h2></header>
                <MediaGrid>{body}</MediaGrid>
            </section>
        }
        .into_any()
    } else {
        view! { <MediaRow title=title>{body}</MediaRow> }.into_any()
    }
}

/// Every radio Discover row of the enabled sources, then their categories as chips (a chosen
/// category opens as a grid), sharing one show dialog.
#[component]
pub fn RadioRows() -> impl IntoView {
    let open = RwSignal::new(false);
    let current = RwSignal::new(None::<ContentItemDto>);
    let category = RwSignal::new(None::<RadioDiscoverListDto>);
    let on_open = Callback::new(move |item: ContentItemDto| {
        current.set(Some(item));
        open.set(true);
    });
    let lists = LocalResource::new(|| api::get::<RadioDiscoverDto>("/discover/genres/radio"));

    let chips = move |cats: Vec<RadioDiscoverListDto>| {
        // Grouped by source and heading (Comedy, Drama), in the order the server lists them.
        let mut groups: Vec<(String, Vec<RadioDiscoverListDto>)> = Vec::new();
        for c in cats {
            let heading = format!(
                "{} · {}",
                c.group.as_deref().unwrap_or("Categories"),
                c.provider_name
            );
            match groups.iter_mut().find(|(h, _)| *h == heading) {
                Some((_, v)) => v.push(c),
                None => groups.push((heading, vec![c])),
            }
        }
        groups
            .into_iter()
            .map(|(heading, cats)| {
                view! {
                    <section class="media-row">
                        <header class="media-row-head"><h2 class="media-row-title">{heading}</h2></header>
                        <div class="chip-row" role="list">
                            {cats.into_iter().map(|c| {
                                let id = c.id.clone();
                                let active = move || category.with(|s| s.as_ref().is_some_and(|s| s.id == id));
                                let label = c.name.clone();
                                let stored = StoredValue::new(c);
                                view! {
                                    <button type="button" class="pill-link" role="listitem"
                                        class:active=active.clone() aria-pressed=move || active().to_string()
                                        on:click=move |_| {
                                            let c = stored.get_value();
                                            category.update(|s| {
                                                *s = if s.as_ref().is_some_and(|x| x.id == c.id) { None } else { Some(c) };
                                            });
                                        }>
                                        {label}
                                    </button>
                                }
                            }).collect_view()}
                        </div>
                    </section>
                }
            })
            .collect_view()
    };

    view! {
        <div class="discover">
            {move || match lists.get() {
                None => view! { <MediaRow title="Radio"><MediaCardSkeleton /></MediaRow> }.into_any(),
                Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| lists.refetch()) /> }.into_any(),
                Some(Ok(l)) if l.rows.is_empty() && l.categories.is_empty() => view! {
                    <EmptyState icon="radio" title="No radio lists"
                        description="None of the radio sources turned on in the radio settings has lists to browse. Search for a show instead." />
                }.into_any(),
                Some(Ok(l)) => view! {
                    {l.rows.into_iter().map(|r| view! { <RadioList list=r on_open=on_open /> }).collect_view()}
                    {chips(l.categories)}
                }.into_any(),
            }}
            {move || category.get().map(|c| view! { <RadioList list=c on_open=on_open grid=true /> })}
        </div>
        <RadioShowModal open=open item=current />
    }
}

/// The radio show dialog: artwork, station, synopsis, newest episodes, requests, and Follow
/// (library managers) or Request.
#[component]
pub fn RadioShowModal(
    open: RwSignal<bool>,
    item: RwSignal<Option<ContentItemDto>>,
    #[prop(optional)] on_changed: Option<Callback<()>>,
) -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let busy = RwSignal::new(false);
    // Show id once followed from this dialog.
    let followed_now = RwSignal::new(None::<i64>);

    let detail = LocalResource::new(move || {
        let path = open
            .get()
            .then(|| item.with(|i| i.as_ref().and_then(|i| detail_path(&i.foreign_id))))
            .flatten();
        async move { Some(api::get::<ContentDetailDto>(&path?).await) }
    });
    Effect::new(move |_| {
        if open.get() {
            followed_now.set(None);
        }
    });
    let quota = LocalResource::new(move || {
        let id = auth.user.with(|u| u.as_ref().map(|u| u.id));
        let want = open.get() && !can_follow(&auth);
        async move {
            match (want, id) {
                (true, Some(id)) => api::get::<QuotaDto>(&format!("/users/{id}/quota")).await.ok(),
                _ => None,
            }
        }
    });
    let changed = move || {
        detail.refetch();
        if let Some(cb) = on_changed {
            cb.run(());
        }
    };

    let follow = move |_| {
        let Some((p, e)) = item.with_untracked(|i| {
            i.as_ref().and_then(|i| {
                split_radio_foreign_id(&i.foreign_id).map(|(p, e)| (p.to_owned(), e.to_owned()))
            })
        }) else {
            return;
        };
        let req = AddRadioShowRequest {
            provider: Some(p),
            external_id: Some(e),
            monitored: true,
            ..AddRadioShowRequest::default()
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, RadioShowDto>("/radio/shows", &req).await {
                Ok(show) => {
                    toasts.success(format!("Following “{}”.", show.title));
                    followed_now.set(Some(show.id));
                    changed();
                }
                Err(e) => toasts.error(format!("Could not follow: {e}")),
            }
            busy.set(false);
        });
    };

    let request = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(foreign_id) = item.with_untracked(|i| i.as_ref().map(|i| i.foreign_id.clone())) else {
            return;
        };
        let req = CreateContentRequestDto {
            kind: ContentRequestKind::Radio,
            foreign_id,
            formats: Vec::new(),
            profile_id: None,
            root_folder_id: None,
            tags: Vec::new(),
            user_id: None,
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, ContentRequestDto>("/requests/media", &req).await {
                Ok(r) => {
                    toasts.success(match r.status {
                        RequestStatus::Pending => {
                            format!("Requested “{}”. An admin will review it.", r.title)
                        }
                        RequestStatus::Failed => format!(
                            "Requested “{}”, but following it failed: {}",
                            r.title,
                            r.failure_reason.clone().unwrap_or_default()
                        ),
                        _ => format!("Requested “{}” — it was approved and is followed now.", r.title),
                    });
                    changed();
                }
                Err(e) => toasts.error(format!("Could not request: {e}")),
            }
            busy.set(false);
        });
    };

    let quota_view = move || {
        let q = quota.get().flatten()?;
        if q.exempt {
            return None;
        }
        let line = quota_line(&q.radio, "radio requests")?;
        let restricted = q.radio.restricted;
        Some(view! {
            <div class="alert" class:alert-warning=restricted class:alert-info=!restricted role="status">
                <Icon name="info" />
                <div class="alert-body"><span>{line}</span></div>
            </div>
        })
    };

    let body = move || match detail.get() {
        None | Some(None) => view! { <MediaCardSkeleton count=1 /> }.into_any(),
        Some(Some(Err(e))) => view! { <p class="text-danger">{e.message}</p> }.into_any(),
        Some(Some(Ok(d))) => {
            let preview = d.radio.clone();
            let show = preview.as_ref().map(|p| p.show.clone());
            let show_id = followed_now
                .get()
                .or(d.item.library_id)
                .or(show.as_ref().and_then(|s| s.show_id));
            let is_followed = show_id.is_some() || d.item.availability == AvailabilityStatus::Available;
            let meta = [
                show.as_ref().and_then(|s| s.station_name.clone()),
                show.as_ref().and_then(|s| s.author.clone()),
                preview
                    .as_ref()
                    .filter(|p| p.episode_count > 0)
                    .map(|p| format!("{} episodes", p.episode_count)),
                preview
                    .as_ref()
                    .and_then(|p| p.first_broadcast)
                    .map(|t| format!("since {}", format::date(t))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            let links = show
                .as_ref()
                .map(|s| {
                    [
                        s.web_url
                            .as_deref()
                            .and_then(format::http_url)
                            .map(|u| ("Show page", u)),
                        s.feed_url
                            .as_deref()
                            .and_then(format::http_url)
                            .map(|u| ("Podcast feed", u)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let episodes = preview
                .as_ref()
                .map(|p| p.latest_episodes.clone())
                .unwrap_or_default();
            let ability = d.request_ability.clone();
            let manager = can_follow(&auth);
            let can_request = ability.can_request && auth.has(Permissions::REQUEST_RADIO);
            let actions = if is_followed {
                view! {
                    <div class="modal-footer">
                        {(manager).then_some(()).and(show_id).map(|id| view! {
                            <A href=format!("/radio/shows/{id}") attr:class="btn btn-secondary">"Open show"</A>
                        })}
                        <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Close"</button>
                    </div>
                }
                .into_any()
            } else if manager {
                view! {
                    <div class="modal-footer">
                        <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                        <button type="button" class="btn btn-primary" disabled=move || busy.get() on:click=follow>
                            <Icon name="radio" />
                            {move || if busy.get() { "Following…" } else { "Follow" }}
                        </button>
                    </div>
                }
                .into_any()
            } else if can_request {
                view! {
                    <form class="form" on:submit=request>
                        {quota_view}
                        <div class="modal-footer">
                            <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                            <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                                <Icon name="requests" />
                                {move || if busy.get() { "Requesting…" } else { "Request" }}
                            </button>
                        </div>
                    </form>
                }
                .into_any()
            } else {
                view! {
                    {ability.reason.clone().map(|r| view! { <p class="muted">{r}</p> })}
                    <div class="modal-footer">
                        <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Close"</button>
                    </div>
                }
                .into_any()
            };
            view! {
                <div class="content-request">
                    <div class="request-title-cell">
                        <span style="width:96px;flex:none"><HitArt url=d.item.cover.clone() /></span>
                        <div>
                            <div class="cell-strong">{d.item.title.clone()}</div>
                            {(!meta.is_empty()).then(|| view! { <div class="cell-sub">{meta}</div> })}
                            <div class="drawer-status">
                                {is_followed.then(|| view! { <span class="badge tone-success">"Following"</span> })}
                                {d.requests.first().map(|r| view! { <RequestStatusBadge status=r.status /> })}
                            </div>
                        </div>
                    </div>
                    {d.overview.clone().map(|o| view! { <p class="modal-text">{o}</p> })}
                    {(!links.is_empty()).then(|| view! {
                        <p class="cell-sub">
                            {links.into_iter().map(|(label, href)| view! {
                                <a href=href target="_blank" rel="noopener noreferrer">{label}</a>" "
                            }).collect_view()}
                        </p>
                    })}
                    {(!episodes.is_empty()).then(|| view! {
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
                                    {episodes.into_iter().map(|e| view! {
                                        <tr>
                                            <td class="mono nowrap">{match (e.series_number, e.number) {
                                                (Some(s), Some(n)) => format!("S{s:02}E{n:02}"),
                                                (None, Some(n)) => format!("E{n:02}"),
                                                _ => "—".into(),
                                            }}</td>
                                            <td>{e.title}</td>
                                            <td class="nowrap">{e.first_broadcast.map(format::date).unwrap_or_else(|| "—".into())}</td>
                                            <td class="nowrap">{length(e.duration_secs)}</td>
                                        </tr>
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    })}
                    {(!d.requests.is_empty()).then(|| view! {
                        <ul class="plain-list">
                            {d.requests.iter().map(|r| view! {
                                <li class="cell-sub">
                                    <RequestStatusBadge status=r.status />
                                    " "
                                    <UserChip user=r.requested_by.clone() />
                                    {format!(" · {}", format::relative(r.created_at))}
                                </li>
                            }).collect_view()}
                        </ul>
                    })}
                    {actions}
                </div>
            }
            .into_any()
        }
    };

    view! {
        <Modal open=open title="Radio show">
            {body}
        </Modal>
    }
}
