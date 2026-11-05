//! `/discover` — the requester home: hero carousel, feed rows, genres, networks and studios —
//! and `/discover/{feed}` ("see all" grids with paging). `/discover?tab=books|music|radio`
//! shows the book / music / radio search and rows (users with REQUEST_BOOK / REQUEST_MUSIC;
//! radio for REQUEST_RADIO or MANAGE_LIBRARY, who follow shows directly).

pub mod content;
pub mod radio;

use std::time::Duration;

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::{use_params_map, use_query_map};
use reelix_api_types::Permissions;
use reelix_api_types::requests::{ContentRequestKind, DiscoverItemDto, DiscoverPageDto, GenreDto};

use crate::api;
use crate::components::media_card::{MediaBadge, MediaCard, MediaCardSkeleton, MediaGrid, MediaRow};
use crate::components::request_status::{item_badge, kind_label, media_href};
use crate::components::{EmptyState, ErrorBox, Icon};
use crate::state::AuthState;

/// Discover rows in display order: (feed, title).
const ROWS: &[(&str, &str)] = &[
    ("trending", "Trending"),
    ("popular-movies", "Popular movies"),
    ("upcoming", "Upcoming movies"),
    ("popular-series", "Popular series"),
    ("top-rated-movies", "Top rated movies"),
    ("top-rated-series", "Top rated series"),
];

/// Well-known TMDB network ids.
const NETWORKS: &[(u32, &str)] = &[
    (213, "Netflix"),
    (49, "HBO"),
    (2552, "Apple TV+"),
    (2739, "Disney+"),
    (1024, "Prime Video"),
    (453, "Hulu"),
    (4, "BBC One"),
    (174, "AMC"),
    (67, "Showtime"),
    (318, "Starz"),
    (19, "FOX"),
    (6, "NBC"),
];

/// Well-known TMDB company (studio) ids.
const STUDIOS: &[(u32, &str)] = &[
    (420, "Marvel Studios"),
    (2, "Walt Disney Pictures"),
    (3, "Pixar"),
    (174, "Warner Bros."),
    (33, "Universal"),
    (4, "Paramount"),
    (41077, "A24"),
    (1, "Lucasfilm"),
    (521, "DreamWorks Animation"),
    (10342, "Studio Ghibli"),
    (25, "20th Century"),
    (5, "Columbia"),
];

fn year_label(item: &DiscoverItemDto) -> Option<String> {
    item.year.map(|y| y.to_string())
}

fn cards(items: Vec<DiscoverItemDto>) -> impl IntoView {
    items
        .into_iter()
        .map(|item| {
            view! {
                <MediaCard
                    href=media_href(item.kind, item.tmdb_id)
                    title=item.title.clone()
                    subtitle=year_label(&item)
                    poster=item.poster.clone()
                    kind_label=kind_label(item.kind).to_string()
                    badge=item_badge(&item)
                />
            }
        })
        .collect_view()
}

fn feed_url(feed: &str, page: u32) -> String {
    format!("/discover/{feed}?page={page}")
}

/// Movies & TV / Books / Music / Radio tabs; books, music and radio only for users who may
/// request (or, for radio, follow) them.
#[component]
pub fn DiscoverPage() -> impl IntoView {
    let auth = AuthState::expect();
    let query = use_query_map();
    let tab = Memo::new(move |_| query.with(|q| q.get("tab").unwrap_or_default()));
    let books = move || auth.has(Permissions::REQUEST_BOOK);
    let music = move || auth.has(Permissions::REQUEST_MUSIC);
    let radio = move || auth.has_any(Permissions::REQUEST_RADIO | Permissions::MANAGE_LIBRARY);
    let tabs = move || {
        (books() || music() || radio()).then(|| {
            let current = tab.get();
            let item = |value: &'static str, label: &'static str, icon: &'static str| {
                let active = current == value
                    || (value.is_empty() && !matches!(current.as_str(), "books" | "music" | "radio"));
                let href = if value.is_empty() {
                    "/discover".to_string()
                } else {
                    format!("/discover?tab={value}")
                };
                view! {
                    <A href=href attr:class=if active { "tab active" } else { "tab" }
                        attr:aria-current=active.then_some("page")>
                        <Icon name=icon />{label}
                    </A>
                }
            };
            view! {
                <nav class="tabs" aria-label="What to discover">
                    {item("", "Movies & TV", "film")}
                    {books().then(|| item("books", "Books", "books"))}
                    {music().then(|| item("music", "Music", "music"))}
                    {radio().then(|| item("radio", "Radio", "radio"))}
                </nav>
            }
        })
    };
    let body = move || match tab.get().as_str() {
        "books" if books() => view! { <content::ContentDiscover kind=ContentRequestKind::Book /> }.into_any(),
        "music" if music() => {
            view! { <content::ContentDiscover kind=ContentRequestKind::Music /> }.into_any()
        }
        "radio" if radio() => {
            view! { <content::ContentDiscover kind=ContentRequestKind::Radio /> }.into_any()
        }

        _ => view! { <MoviesTvDiscover /> }.into_any(),
    };
    view! {
        {tabs}
        {body}
    }
}

/// The movie / series discover home.
#[component]
fn MoviesTvDiscover() -> impl IntoView {
    let trending =
        LocalResource::new(|| async { api::get::<DiscoverPageDto>(&feed_url("trending", 1)).await });
    let movie_genres = LocalResource::new(|| api::get::<Vec<GenreDto>>("/discover/genres/movie"));
    let tv_genres = LocalResource::new(|| api::get::<Vec<GenreDto>>("/discover/genres/series"));

    let hero = move || match trending.get() {
        None => view! { <div class="hero hero-loading skeleton" aria-hidden="true"></div> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| trending.refetch()) /> }.into_any()
        }
        Some(Ok(page)) => {
            let items: Vec<_> = page
                .results
                .into_iter()
                .filter(|i| i.backdrop.is_some())
                .take(6)
                .collect();
            if items.is_empty() {
                view! {
                    <EmptyState icon="discover" title="Nothing to discover yet"
                        description="Discovery needs a TMDB API key — an admin can add one under Settings → Metadata." />
                }
                .into_any()
            } else {
                view! { <Hero items=items /> }.into_any()
            }
        }
    };

    let genre_chips = move |kind: &'static str, res: LocalResource<api::ApiResult<Vec<GenreDto>>>| {
        move || {
            let genres = res.get().and_then(Result::ok).unwrap_or_default();
            (!genres.is_empty()).then(|| {
                view! {
                    <div class="chip-row" role="list">
                        {genres
                            .into_iter()
                            .map(|g| {
                                view! {
                                    <A href=format!("/discover/genre/{kind}/{}", g.id) attr:class="pill-link" attr:role="listitem">
                                        {g.name}
                                    </A>
                                }
                            })
                            .collect_view()}
                    </div>
                }
            })
        }
    };

    view! {
        <div class="discover">
            {hero}
            {ROWS
                .iter()
                .enumerate()
                .map(|(i, (feed, title))| {
                    view! {
                        <FeedRow feed=*feed title=*title preloaded=(i == 0).then_some(trending) />
                    }
                })
                .collect_view()}
            <section class="media-row">
                <header class="media-row-head"><h2 class="media-row-title">"Movie genres"</h2></header>
                {genre_chips("movie", movie_genres)}
            </section>
            <section class="media-row">
                <header class="media-row-head"><h2 class="media-row-title">"Series genres"</h2></header>
                {genre_chips("series", tv_genres)}
            </section>
            <section class="media-row">
                <header class="media-row-head"><h2 class="media-row-title">"Networks"</h2></header>
                <div class="brand-row" role="list">
                    {NETWORKS
                        .iter()
                        .map(|(id, name)| view! {
                            <A href=format!("/discover/network/{id}") attr:class="brand-tile" attr:role="listitem">{*name}</A>
                        })
                        .collect_view()}
                </div>
            </section>
            <section class="media-row">
                <header class="media-row-head"><h2 class="media-row-title">"Studios"</h2></header>
                <div class="brand-row" role="list">
                    {STUDIOS
                        .iter()
                        .map(|(id, name)| view! {
                            <A href=format!("/discover/studio/{id}") attr:class="brand-tile" attr:role="listitem">{*name}</A>
                        })
                        .collect_view()}
                </div>
            </section>
        </div>
    }
}

/// A horizontally scrolling row for one feed.
#[component]
fn FeedRow(
    feed: &'static str,
    title: &'static str,
    /// Reuse an already running request for this feed (the hero shares "trending").
    preloaded: Option<LocalResource<api::ApiResult<DiscoverPageDto>>>,
) -> impl IntoView {
    let res = preloaded.unwrap_or_else(|| {
        LocalResource::new(move || async move { api::get::<DiscoverPageDto>(&feed_url(feed, 1)).await })
    });
    let body = move || match res.get() {
        None => view! { <MediaCardSkeleton /> }.into_any(),
        Some(Err(e)) => view! { <p class="muted row-error">{e.message}</p> }.into_any(),
        Some(Ok(page)) if page.results.is_empty() => {
            view! { <p class="muted row-error">"Nothing here right now."</p> }.into_any()
        }
        Some(Ok(page)) => cards(page.results).into_any(),
    };
    view! {
        <MediaRow title=title see_all=format!("/discover/{feed}")>
            {body}
        </MediaRow>
    }
}

/// Auto-rotating backdrop carousel.
#[component]
fn Hero(items: Vec<DiscoverItemDto>) -> impl IntoView {
    let count = items.len();
    let index = RwSignal::new(0usize);
    // Hover/focus pause temporarily; the button (and reduced motion) stops rotation for good.
    let paused = RwSignal::new(false);
    let reduced_motion = window()
        .match_media("(prefers-reduced-motion: reduce)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches());
    let stopped = RwSignal::new(reduced_motion);
    let handle = set_interval_with_handle(
        move || {
            if !paused.get_untracked() && !stopped.get_untracked() {
                index.update(|i| *i = (*i + 1) % count);
            }
        },
        Duration::from_secs(8),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = handle {
            h.clear();
        }
    });
    let items = StoredValue::new(items);

    let slide = move || {
        let item = items.with_value(|v| v[index.get() % count].clone());
        let backdrop = item.backdrop.clone().unwrap_or_default();
        let href = media_href(item.kind, item.tmdb_id);
        let meta = [
            Some(kind_label(item.kind).to_string()),
            item.year.map(|y| y.to_string()),
            (item.vote_count > 0).then(|| format!("★ {:.1}", item.vote_average)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let badge = item_badge(&item);
        view! {
            <div class="hero-slide">
                <img class="hero-backdrop" src=backdrop alt="" />
                <div class="hero-shade"></div>
                <div class="hero-content">
                    <span class="hero-eyebrow">"Trending now"</span>
                    <h2 class="hero-title">{item.title.clone()}</h2>
                    <p class="hero-meta">{meta} {badge.map(|b| view! { <MediaBadge kind=b large=true /> })}</p>
                    {item.overview.clone().map(|o| view! { <p class="hero-overview">{o}</p> })}
                    <div class="hero-actions">
                        <A href=href attr:class="btn btn-primary btn-lg">
                            {if badge.is_some() { "View details" } else { "Request" }}
                        </A>
                    </div>
                </div>
            </div>
        }
    };

    view! {
        <section class="hero" aria-roledescription="carousel" aria-label="Trending"
            on:mouseenter=move |_| paused.set(true) on:mouseleave=move |_| paused.set(false)
            on:focusin=move |_| paused.set(true) on:focusout=move |_| paused.set(false)>
            {slide}
            <button type="button" class="hero-pause icon-btn icon-btn-sm"
                aria-pressed=move || stopped.get().to_string()
                aria-label=move || if stopped.get() { "Play slideshow" } else { "Pause slideshow" }
                on:click=move |_| stopped.update(|s| *s = !*s)>
                <Show when=move || stopped.get() fallback=|| view! { <span aria-hidden="true">"❚❚"</span> }>
                    <Icon name="play" />
                </Show>
            </button>
            <button type="button" class="hero-nav prev" aria-label="Previous slide"
                on:click=move |_| index.update(|i| *i = (*i + count - 1) % count)>
                <Icon name="chevron" class="flip" />
            </button>
            <button type="button" class="hero-nav next" aria-label="Next slide"
                on:click=move |_| index.update(|i| *i = (*i + 1) % count)>
                <Icon name="chevron" />
            </button>
            <div class="hero-dots">
                {(0..count)
                    .map(|i| view! {
                        <button type="button" class="hero-dot" class:active=move || index.get() == i
                            aria-label=format!("Show slide {}", i + 1)
                            aria-current=move || (index.get() == i).then_some("true")
                            on:click=move |_| index.set(i)></button>
                    })
                    .collect_view()}
            </div>
        </section>
    }
}

fn feed_title(feed: &str) -> String {
    if let Some((_, t)) = ROWS.iter().find(|(f, _)| *f == feed) {
        return (*t).to_string();
    }
    let parts: Vec<&str> = feed.split('/').collect();
    match parts.as_slice() {
        ["network", id] => NETWORKS
            .iter()
            .find(|(n, _)| n.to_string() == *id)
            .map(|(_, name)| format!("Series on {name}"))
            .unwrap_or_else(|| "Network".into()),
        ["studio", id] => STUDIOS
            .iter()
            .find(|(n, _)| n.to_string() == *id)
            .map(|(_, name)| format!("Movies from {name}"))
            .unwrap_or_else(|| "Studio".into()),
        ["genre", "movie", _] => "Movies by genre".into(),
        ["genre", _, _] => "Series by genre".into(),
        _ => "Discover".into(),
    }
}

/// `/discover/{feed}` — every page of one feed in a grid.
#[component]
pub fn DiscoverFeedPage() -> impl IntoView {
    let params = use_params_map();
    let feed = Memo::new(move |_| params.with(|p| p.get("feed").unwrap_or_default()));
    let items = RwSignal::new(Vec::<DiscoverItemDto>::new());
    let page = RwSignal::new(1u32);
    let total_pages = RwSignal::new(1u32);
    let loading = RwSignal::new(false);
    let error = RwSignal::new(None::<api::ApiError>);

    // Genre feeds get their name from the genre list.
    let genre_name = LocalResource::new(move || {
        let f = feed.get();
        async move {
            let parts: Vec<String> = f.split('/').map(str::to_string).collect();
            if parts.len() == 3 && parts[0] == "genre" {
                let id: u32 = parts[2].parse().ok()?;
                let genres = api::get::<Vec<GenreDto>>(&format!("/discover/genres/{}", parts[1]))
                    .await
                    .ok()?;
                let kind = if parts[1] == "movie" { "movies" } else { "series" };
                genres
                    .into_iter()
                    .find(|g| g.id == id)
                    .map(|g| format!("{} {kind}", g.name))
            } else {
                None
            }
        }
    });
    let title = move || {
        genre_name
            .get()
            .flatten()
            .unwrap_or_else(|| feed_title(&feed.get()))
    };

    let load = move |p: u32| {
        let f = feed.get_untracked();
        loading.set(true);
        leptos::task::spawn_local(async move {
            match api::get::<DiscoverPageDto>(&feed_url(&f, p)).await {
                Ok(res) => {
                    // Ignore late answers for a feed the user already left.
                    if feed.get_untracked() == f {
                        total_pages.set(res.total_pages.max(1));
                        page.set(p);
                        items.update(|v| {
                            if p == 1 {
                                v.clear();
                            }
                            for item in res.results {
                                if !v.iter().any(|x| x.kind == item.kind && x.tmdb_id == item.tmdb_id) {
                                    v.push(item);
                                }
                            }
                        });
                        error.set(None);
                    }
                }
                Err(e) => error.set(Some(e)),
            }
            loading.set(false);
        });
    };
    Effect::new(move |_| {
        feed.track();
        items.set(Vec::new());
        load(1);
    });

    view! {
        <div class="page-header">
            <div>
                <h1 class="page-title">{title}</h1>
            </div>
            <div class="page-actions">
                <A href="/discover" attr:class="btn btn-secondary btn-sm">"Back to Discover"</A>
            </div>
        </div>
        {move || error.get().map(|e| view! { <ErrorBox error=e on_retry=Callback::new(move |()| load(page.get_untracked())) /> })}
        <MediaGrid>
            {move || cards(items.get())}
            {move || (loading.get() && items.with(Vec::is_empty)).then(|| view! { <MediaCardSkeleton count=12 /> })}
        </MediaGrid>
        {move || (!loading.get() && error.get().is_none() && items.with(Vec::is_empty)).then(|| view! {
            <EmptyState icon="discover" title="Nothing here" description="This feed has no titles right now." />
        })}
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
