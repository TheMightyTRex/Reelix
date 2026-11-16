//! Books (Open Library) and music (MusicBrainz) on Discover / Search: browsable rows (Open
//! Library trending and subjects, ListenBrainz new releases and charts), search results with
//! availability badges and the book/album request dialog (format picker for books). Radio has
//! its own cards and show dialog in [`super::radio`]; the components here hand radio to it.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::Permissions;
use reelix_api_types::requests::{
    ArtistItemDto, AvailabilityStatus, BOOK_SUBJECTS, BookSearchPageDto, BookSubject, BookSubjectGroup,
    ContentDetailDto, ContentItemDto, ContentRequestDto, ContentRequestKind, ContentRowDto,
    CreateContentRequestDto, MusicSearchPageDto, QuotaDto, RadioSearchPageDto, RequestOptionsDto,
    RequestStatus,
};
use reelix_core::BookFormat;

use crate::api;
use crate::components::media_card::{
    BadgeKind, MediaBadge, MediaCardSkeleton, MediaGrid, MediaRow, tmdb_image,
};
use crate::components::request_modal::quota_line;
use crate::components::request_status::{
    RequestStatusBadge, UserChip, availability_label, refetch_on_change, url_encode,
};
use crate::components::{EmptyState, ErrorBox, Icon, Modal};
use crate::format;
use crate::state::{AuthState, Toasts};

/// Resets the default `<button>` look so a card button renders like the poster links.
pub(crate) const CARD_BUTTON_STYLE: &str = "background:none;border:0;padding:0;margin:0;text-align:left;font:inherit;color:inherit;cursor:pointer;width:100%";

pub fn format_label(f: BookFormat) -> &'static str {
    match f {
        BookFormat::Ebook => "Ebook",
        BookFormat::Audiobook => "Audiobook",
    }
}

/// `Ebook + Audiobook`, or empty for albums.
pub fn formats_label(formats: &[BookFormat]) -> String {
    formats
        .iter()
        .map(|f| format_label(*f))
        .collect::<Vec<_>>()
        .join(" + ")
}

pub fn content_kind_label(k: ContentRequestKind) -> &'static str {
    match k {
        ContentRequestKind::Book => "Book",
        ContentRequestKind::Music => "Album",
        ContentRequestKind::Radio => "Radio",
    }
}

/// Permission needed to request this kind.
pub fn content_permission(k: ContentRequestKind) -> Permissions {
    match k {
        ContentRequestKind::Book => Permissions::REQUEST_BOOK,
        ContentRequestKind::Music => Permissions::REQUEST_MUSIC,
        ContentRequestKind::Radio => Permissions::REQUEST_RADIO,
    }
}

/// Poster badge for a book/album: library state first, then the request.
pub fn content_badge(item: &ContentItemDto) -> Option<BadgeKind> {
    match item.availability {
        AvailabilityStatus::Available => return Some(BadgeKind::Available),
        AvailabilityStatus::PartiallyAvailable => return Some(BadgeKind::Partial),
        AvailabilityStatus::Processing => return Some(BadgeKind::Processing),
        _ => {}
    }
    match item.request.as_ref().map(|r| r.status)? {
        RequestStatus::Pending | RequestStatus::Approved => Some(BadgeKind::Requested),
        RequestStatus::Processing => Some(BadgeKind::Processing),
        RequestStatus::PartiallyAvailable => Some(BadgeKind::Partial),
        RequestStatus::Available => Some(BadgeKind::Available),
        RequestStatus::Declined | RequestStatus::Failed => None,
    }
}

/// One clickable cover tile.
#[component]
fn ContentCard(item: ContentItemDto, on_open: Callback<ContentItemDto>) -> impl IntoView {
    let src = tmdb_image(item.cover.as_deref(), "w342");
    let initials: String = item
        .title
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect();
    let sub = [item.subtitle.clone(), item.year.map(|y| y.to_string())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let badge = content_badge(&item);
    let tag = item
        .album_type
        .clone()
        .unwrap_or_else(|| content_kind_label(item.kind).to_string());
    let icon = match item.kind {
        ContentRequestKind::Book => "books",
        ContentRequestKind::Music => "music",
        ContentRequestKind::Radio => "radio",
    };
    let label = format!("{} — {}", item.title, sub);
    let title = item.title.clone();
    let stored = StoredValue::new(item);
    view! {
        <button type="button" class="media-card" style=CARD_BUTTON_STYLE aria-label=label
            on:click=move |_| on_open.run(stored.get_value())>
            <div class="media-poster">
                {match src {
                    Some(src) => view! { <img src=src alt="" loading="lazy" decoding="async" /> }.into_any(),
                    None => view! { <span class="media-poster-fallback" aria-hidden="true">{initials}</span> }.into_any(),
                }}
                <span class="media-kind"><Icon name=icon />{tag}</span>
                {badge.map(|b| view! { <MediaBadge kind=b /> })}
            </div>
            <div class="media-meta">
                <span class="media-title">{title}</span>
                {(!sub.is_empty()).then(|| view! { <span class="media-sub">{sub.clone()}</span> })}
            </div>
        </button>
    }
}

/// Search results for books, albums (+ artists) or radio shows of `query`, with the request
/// dialog.
#[component]
pub fn ContentResults(kind: ContentRequestKind, #[prop(into)] query: Signal<String>) -> impl IntoView {
    if kind == ContentRequestKind::Radio {
        return view! { <super::radio::RadioResults query=query /> }.into_any();
    }
    let open = RwSignal::new(false);
    let current = RwSignal::new(None::<ContentItemDto>);
    let artists = RwSignal::new(Vec::<ArtistItemDto>::new());
    let res = LocalResource::new(move || {
        let q = query.get().trim().to_string();
        async move {
            if q.is_empty() {
                return Ok(None);
            }
            let path = match kind {
                ContentRequestKind::Book => "/search/books",
                ContentRequestKind::Music => "/search/music",
                ContentRequestKind::Radio => "/search/radio",
            };
            let url = format!("{path}?q={}", url_encode(&q));
            match kind {
                ContentRequestKind::Book => api::get::<BookSearchPageDto>(&url)
                    .await
                    .map(|p| Some((p.results, Vec::new()))),
                ContentRequestKind::Music => api::get::<MusicSearchPageDto>(&url)
                    .await
                    .map(|p| Some((p.albums, p.artists))),
                ContentRequestKind::Radio => api::get::<RadioSearchPageDto>(&url)
                    .await
                    .map(|p| Some((p.results, Vec::new()))),
            }
        }
    });
    refetch_on_change(&["request"], move || res.refetch());
    Effect::new(move |_| {
        if let Some(Ok(Some((_, a)))) = res.get() {
            artists.set(a);
        } else {
            artists.set(Vec::new());
        }
    });
    let on_open = Callback::new(move |item: ContentItemDto| {
        current.set(Some(item));
        open.set(true);
    });
    let noun = match kind {
        ContentRequestKind::Book => "books",
        ContentRequestKind::Music => "albums",
        ContentRequestKind::Radio => "radio shows",
    };

    let artist_chips = move || {
        let list = artists.get();
        (!list.is_empty()).then(|| {
            view! {
                <div class="people-row">
                    <span class="field-label">"Artists"</span>
                    <div class="chip-list">
                        {list.into_iter().map(|a| {
                            let mut title = [a.disambiguation.clone(), a.artist_type.clone(), a.country.clone()]
                                .into_iter().flatten().collect::<Vec<_>>().join(" · ");
                            if a.library_id.is_some() {
                                title = if title.is_empty() { "In library".into() } else { format!("{title} · In library") };
                            }
                            view! {
                                <span class="chip" title=title>
                                    {a.name}
                                    {a.library_id.map(|_| view! { <span class="badge tone-success badge-xs">"In library"</span> })}
                                </span>
                            }
                        }).collect_view()}
                    </div>
                </div>
            }
        })
    };

    view! {
        {artist_chips}
        {move || match res.get() {
            None => view! { <MediaGrid><MediaCardSkeleton count=12 /></MediaGrid> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| res.refetch()) /> }.into_any(),
            Some(Ok(None)) => view! {
                <EmptyState icon="search" title=format!("Search for {noun}")
                    description=match kind {
                        ContentRequestKind::Book => "Find books on Open Library by title, author or ISBN and request the ebook or audiobook.",
                        ContentRequestKind::Music => "Find albums on MusicBrainz by title or artist and request them.",
                        ContentRequestKind::Radio => "Find radio programmes and podcasts by name or link.",
                    } />
            }.into_any(),
            Some(Ok(Some((items, _)))) if items.is_empty() => view! {
                <EmptyState icon="search" title="No results" description="Try a different spelling or search term." />
            }.into_any(),
            Some(Ok(Some((items, _)))) => view! {
                <MediaGrid>
                    {items.into_iter().map(|item| view! { <ContentCard item=item on_open=on_open /> }).collect_view()}
                </MediaGrid>
            }.into_any(),
        }}
        <ContentRequestModal open=open kind=kind item=current on_requested=Callback::new(move |_| res.refetch()) />
    }
    .into_any()
}

/// The book/album request view: cover, availability, existing requests, format picker
/// (books), advanced options and the request button.
#[component]
pub fn ContentRequestModal(
    open: RwSignal<bool>,
    kind: ContentRequestKind,
    /// The item to show (`foreign_id` is what gets requested).
    item: RwSignal<Option<ContentItemDto>>,
    #[prop(optional)] on_requested: Option<Callback<ContentRequestDto>>,
) -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let formats = RwSignal::new(Vec::<BookFormat>::new());
    let advanced = RwSignal::new(false);
    let profile_id = RwSignal::new(String::new());
    let root_id = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let detail = LocalResource::new(move || {
        let id = open
            .get()
            .then(|| item.with(|i| i.as_ref().map(|i| i.foreign_id.clone())))
            .flatten();
        async move {
            let id = id?;
            let seg = match kind {
                ContentRequestKind::Book => "book",
                ContentRequestKind::Music => "album",
                // Radio details live at /media/radio/{provider}/{id} (see `super::radio`).
                ContentRequestKind::Radio => "radio",
            };
            Some(api::get::<ContentDetailDto>(&format!("/media/{seg}/{}", url_encode(&id))).await)
        }
    });
    // Preselect the default formats whenever a detail arrives.
    Effect::new(move |_| {
        if let Some(Some(Ok(d))) = detail.get() {
            formats.set(d.request_ability.default_formats.clone());
            advanced.set(false);
            profile_id.set(String::new());
            root_id.set(String::new());
        }
    });

    // Advanced options: books only for a single format.
    let option_kind = move || match kind {
        ContentRequestKind::Music => Some("music"),
        ContentRequestKind::Radio => Some("radio"),
        ContentRequestKind::Book => match formats.get().as_slice() {
            [BookFormat::Ebook] => Some("ebook"),
            [BookFormat::Audiobook] => Some("audiobook"),
            _ => None,
        },
    };
    let options = LocalResource::new(move || {
        let want = open.get() && advanced.get();
        let k = option_kind();
        async move {
            match (want, k) {
                (true, Some(k)) => {
                    Some(api::get::<RequestOptionsDto>(&format!("/requests/media/options?kind={k}")).await)
                }
                _ => None,
            }
        }
    });
    Effect::new(move |_| {
        if let Some(Some(Ok(o))) = options.get() {
            profile_id.set(
                o.default_profile_id
                    .or(o.profiles.first().map(|p| p.id))
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
            );
            root_id.set(
                o.default_root_folder_id
                    .or(o.root_folders.first().map(|r| r.id))
                    .map(|r| r.to_string())
                    .unwrap_or_default(),
            );
        }
    });

    let quota = LocalResource::new(move || {
        let id = auth.user.with(|u| u.as_ref().map(|u| u.id));
        let want = open.get();
        async move {
            match (want, id) {
                (true, Some(id)) => api::get::<QuotaDto>(&format!("/users/{id}/quota")).await.ok(),
                _ => None,
            }
        }
    });
    let quota_view = move || {
        let q = quota.get().flatten()?;
        if q.exempt {
            return None;
        }
        let (usage, noun) = match kind {
            ContentRequestKind::Book => (q.book, "book requests"),
            ContentRequestKind::Music => (q.music, "album requests"),
            ContentRequestKind::Radio => (q.radio, "radio requests"),
        };
        let line = quota_line(&usage, noun)?;
        Some(view! {
            <div class="alert" class:alert-warning=usage.restricted class:alert-info=!usage.restricted role="status">
                <Icon name="info" />
                <div class="alert-body"><span>{line}</span></div>
            </div>
        })
    };

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(Some(Ok(d))) = detail.get_untracked() else {
            return;
        };
        let chosen = formats.get_untracked();
        if kind == ContentRequestKind::Book && chosen.is_empty() {
            toasts.error("Pick at least one format.");
            return;
        }
        let adv = advanced.get_untracked() && option_kind().is_some();
        let req = CreateContentRequestDto {
            kind,
            foreign_id: d.item.foreign_id.clone(),
            formats: if kind == ContentRequestKind::Book {
                chosen
            } else {
                Vec::new()
            },
            profile_id: adv.then(|| profile_id.get_untracked().parse().ok()).flatten(),
            root_folder_id: adv.then(|| root_id.get_untracked().parse().ok()).flatten(),
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
                            "Requested “{}”, but adding it failed: {}",
                            r.title,
                            r.failure_reason.clone().unwrap_or_default()
                        ),
                        _ => format!("Requested “{}” — it was approved automatically.", r.title),
                    });
                    open.set(false);
                    if let Some(cb) = on_requested {
                        cb.run(r);
                    }
                }
                Err(e) => toasts.error(format!("Could not request: {e}")),
            }
            busy.set(false);
        });
    };

    let format_picker = move |d: &ContentDetailDto| {
        (kind == ContentRequestKind::Book && d.request_ability.can_request).then(|| {
            let allowed = d.request_ability.formats.clone();
            view! {
                <fieldset class="field">
                    <legend class="field-label">"Formats"</legend>
                    {BookFormat::ALL.into_iter().map(|f| {
                        let ok = allowed.contains(&f);
                        view! {
                            <label class="checkbox" class:disabled=!ok>
                                <input type="checkbox" disabled=!ok
                                    prop:checked=move || ok && formats.with(|v| v.contains(&f))
                                    on:change=move |e| {
                                        let on = event_target_checked(&e);
                                        formats.update(|v| {
                                            v.retain(|x| *x != f);
                                            if on { v.push(f); v.sort(); }
                                        });
                                    } />
                                <span>{format_label(f)}</span>
                                {(!ok).then(|| view! { <span class="badge tone-neutral badge-xs">"Requested"</span> })}
                            </label>
                        }
                    }).collect_view()}
                </fieldset>
            }
        })
    };

    let advanced_fields = move || {
        if !advanced.get() {
            return None;
        }
        if option_kind().is_none() {
            return Some(view! { <p class="muted">"Pick a single format to choose its quality profile and root folder."</p> }.into_any());
        }
        Some(match options.get() {
            None | Some(None) => view! { <p class="muted">"Loading options…"</p> }.into_any(),
            Some(Some(Err(e))) => view! { <p class="text-danger">{e.message}</p> }.into_any(),
            Some(Some(Ok(o))) => view! {
                <div class="field-row">
                    <div class="field">
                        <label for="creq-profile">"Quality profile"</label>
                        <select id="creq-profile" class="input" prop:value=move || profile_id.get()
                            on:change=move |e| profile_id.set(event_target_value(&e))>
                            {o.profiles.iter().map(|p| view! { <option value=p.id.to_string()>{p.name.clone()}</option> }).collect_view()}
                        </select>
                    </div>
                    <div class="field">
                        <label for="creq-root">"Root folder"</label>
                        <select id="creq-root" class="input" prop:value=move || root_id.get()
                            on:change=move |e| root_id.set(event_target_value(&e))>
                            {o.root_folders.iter().map(|r| view! { <option value=r.id.to_string()>{r.path.clone()}</option> }).collect_view()}
                        </select>
                    </div>
                </div>
            }.into_any(),
        })
    };

    let body = move || match detail.get() {
        None | Some(None) => view! { <MediaCardSkeleton count=1 /> }.into_any(),
        Some(Some(Err(e))) => view! { <p class="text-danger">{e.message}</p> }.into_any(),
        Some(Some(Ok(d))) => {
            let cover = tmdb_image(d.item.cover.as_deref(), "w342");
            let meta = [
                d.item.subtitle.clone().map(|s| format!("by {s}")),
                d.item.year.map(|y| y.to_string()),
                d.item.album_type.clone(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            let ability = d.request_ability.clone();
            let can = ability.can_request && auth.has(content_permission(kind));
            let picker = format_picker(&d);
            let in_library = d.item.availability != AvailabilityStatus::Unknown;
            view! {
                <div class="content-request">
                    <div class="request-title-cell">
                        <span class="rq-thumb" style="width:72px;height:auto">
                            {cover.map(|src| view! { <img src=src alt="" /> })}
                        </span>
                        <div>
                            <div class="cell-strong">{d.item.title.clone()}</div>
                            <div class="cell-sub">{meta}</div>
                            <div class="drawer-status">
                                {in_library.then(|| view! {
                                    <span class="badge tone-info">{availability_label(d.item.availability)}</span>
                                })}
                                {d.requests.first().map(|r| view! { <RequestStatusBadge status=r.status /> })}
                            </div>
                        </div>
                    </div>
                    {d.overview.clone().map(|o| view! { <p class="modal-text">{o}</p> })}
                    {(!d.requests.is_empty()).then(|| view! {
                        <ul class="plain-list">
                            {d.requests.iter().map(|r| {
                                let f = formats_label(&r.formats);
                                view! {
                                    <li class="cell-sub">
                                        <RequestStatusBadge status=r.status />
                                        " "
                                        <UserChip user=r.requested_by.clone() />
                                        {(!f.is_empty()).then(|| format!(" · {f}"))}
                                        {format!(" · {}", format::relative(r.created_at))}
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    })}
                    {if can {
                        view! {
                            <form class="form" on:submit=submit>
                                {quota_view}
                                {picker}
                                {ability.can_request_advanced.then(|| view! {
                                    <label class="checkbox">
                                        <input type="checkbox" prop:checked=move || advanced.get()
                                            on:change=move |e| advanced.set(event_target_checked(&e)) />
                                        <span>"Advanced: choose quality profile and root folder"</span>
                                    </label>
                                    {advanced_fields}
                                })}
                                <div class="modal-footer">
                                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                                        <Icon name="requests" />
                                        {move || if busy.get() { "Requesting…" } else { "Request" }}
                                    </button>
                                </div>
                            </form>
                        }.into_any()
                    } else {
                        view! {
                            {ability.reason.clone().map(|r| view! { <p class="muted">{r}</p> })}
                            <div class="modal-footer">
                                <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Close"</button>
                            </div>
                        }.into_any()
                    }}
                </div>
            }
            .into_any()
        }
    };

    let title = match kind {
        ContentRequestKind::Book => "Request book",
        ContentRequestKind::Music => "Request album",
        ContentRequestKind::Radio => "Request radio show",
    };
    view! {
        <Modal open=open title=title>
            {body}
        </Modal>
    }
}

/// One Discover row of books or albums; cards open the shared request dialog via `on_open`.
#[component]
fn ContentRow(
    kind: ContentRequestKind,
    row: &'static str,
    title: &'static str,
    on_open: Callback<ContentItemDto>,
) -> impl IntoView {
    let res = LocalResource::new(move || async move {
        api::get::<ContentRowDto>(&format!("/discover/content/{}/{row}", kind.as_str())).await
    });
    refetch_on_change(&["request"], move || res.refetch());
    let body = move || match res.get() {
        None => view! { <MediaCardSkeleton /> }.into_any(),
        Some(Err(e)) => view! { <p class="muted row-error">{e.message}</p> }.into_any(),
        Some(Ok(page)) if page.results.is_empty() => {
            view! { <p class="muted row-error">"Nothing here right now."</p> }.into_any()
        }
        Some(Ok(page)) => page
            .results
            .into_iter()
            .map(|item| view! { <ContentCard item=item on_open=on_open /> })
            .collect_view()
            .into_any(),
    };
    view! { <MediaRow title=title>{body}</MediaRow> }
}

/// The book subjects that are not default rows, as chips under "More fiction" and
/// "More non-fiction"; a picked subject shows as a row below them.
#[component]
fn BookSubjectChips(on_open: Callback<ContentItemDto>) -> impl IntoView {
    let picked = RwSignal::new(None::<&'static BookSubject>);
    let group = move |group: BookSubjectGroup, heading: &'static str| {
        view! {
            <section class="media-row">
                <header class="media-row-head"><h2 class="media-row-title">{heading}</h2></header>
                <div class="chip-row" role="list">
                    {BOOK_SUBJECTS
                        .iter()
                        .filter(|s| !s.featured && s.group == group)
                        .map(|subject| {
                            let active = move || picked.get().is_some_and(|p| p.id == subject.id);
                            view! {
                                <button type="button" class="pill-link" role="listitem"
                                    class:active=active aria-pressed=move || active().to_string()
                                    on:click=move |_| picked.update(|p| {
                                        *p = if p.is_some_and(|x| x.id == subject.id) { None } else { Some(subject) };
                                    })>
                                    {subject.name}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </section>
        }
    };
    view! {
        {group(BookSubjectGroup::Fiction, "More fiction")}
        {group(BookSubjectGroup::NonFiction, "More non-fiction")}
        {move || picked.get().map(|s| view! {
            <ContentRow kind=ContentRequestKind::Book row=s.id title=s.name on_open=on_open />
        })}
    }
}

/// Every Discover row of `kind`, sharing one request dialog.
#[component]
fn ContentRows(kind: ContentRequestKind) -> impl IntoView {
    let open = RwSignal::new(false);
    let current = RwSignal::new(None::<ContentItemDto>);
    let on_open = Callback::new(move |item: ContentItemDto| {
        current.set(Some(item));
        open.set(true);
    });
    view! {
        <div class="discover">
            {kind
                .discover_rows()
                .into_iter()
                .map(|(row, title)| view! { <ContentRow kind=kind row=row title=title on_open=on_open /> })
                .collect_view()}
            {(kind == ContentRequestKind::Book).then(|| view! { <BookSubjectChips on_open=on_open /> })}
        </div>
        <ContentRequestModal open=open kind=kind item=current />
    }
}

/// Discover's Books / Music tab: a search box, with browsable rows while it is empty and
/// [`ContentResults`] once something is searched.
#[component]
pub fn ContentDiscover(kind: ContentRequestKind) -> impl IntoView {
    let input = RwSignal::new(String::new());
    let query = RwSignal::new(String::new());
    let (label, placeholder) = match kind {
        ContentRequestKind::Book => ("Search books", "Title, author or ISBN…"),
        ContentRequestKind::Music => ("Search music", "Album or artist…"),
        ContentRequestKind::Radio => ("Search radio", "Show, podcast or a link to one…"),
    };
    let id = format!("content-search-{}", kind.as_str());
    view! {
        <form class="search-page-form" role="search"
            on:submit=move |e: ev::SubmitEvent| { e.prevent_default(); query.set(input.get_untracked().trim().to_string()); }>
            <label class="visually-hidden" for=id.clone()>{label}</label>
            <input id=id class="input" type="search" placeholder=placeholder autocomplete="off"
                prop:value=move || input.get()
                on:input=move |e| {
                    let v = event_target_value(&e);
                    // Clearing the box (or its × button) goes back to the rows.
                    if v.trim().is_empty() {
                        query.set(String::new());
                    }
                    input.set(v);
                } />
            <button type="submit" class="btn btn-primary">
                <Icon name="search" />
                "Search"
            </button>
        </form>
        <Show when=move || query.with(String::is_empty) fallback=move || view! { <ContentResults kind=kind query=query /> }>
            {if kind == ContentRequestKind::Radio {
                view! { <super::radio::RadioRows /> }.into_any()
            } else {
                view! { <ContentRows kind=kind /> }.into_any()
            }}
        </Show>

    }
}
