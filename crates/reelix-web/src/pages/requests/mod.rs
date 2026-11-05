//! `/requests` — every request the user may see, with filters, inline approve/decline,
//! bulk actions and a detail drawer. Requesters only see their own. The type filter also
//! switches to book / album requests ([`content`]).

pub mod content;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::Permissions;
use reelix_api_types::requests::{
    DeclineRequestDto, MediaDetailDto, RequestCountsDto, RequestDto, RequestOptionsDto, RequestPageDto,
    RequestStatus, UpdateRequestDto, UserDetailDto,
};
use reelix_core::MediaKind;

use crate::api;
use crate::components::infinite::{InfiniteSentinel, PagedList};
use crate::components::media_card::tmdb_image;
use crate::components::request_drawer::Drawer;
use crate::components::request_status::{
    RequestStatusBadge, UserChip, kind_label, media_href, refetch_on_change, request_status_label,
};
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton};
use crate::format;
use crate::state::{AuthState, Toasts};

pub(crate) const PAGE_SIZE: u32 = 25;

/// Status filter tabs: (query value, label).
const TABS: &[(&str, &str)] = &[
    ("all", "All"),
    ("pending", "Pending"),
    ("active", "Active"),
    ("processing", "Processing"),
    ("available", "Available"),
    ("declined", "Declined"),
    ("failed", "Failed"),
];

fn tab_count(c: &RequestCountsDto, tab: &str) -> Option<u64> {
    Some(match tab {
        "all" => c.total,
        "pending" => c.pending,
        "active" => c.pending + c.approved + c.processing,
        "processing" => c.processing,
        "available" => c.available + c.partially_available,
        "declined" => c.declined,
        "failed" => c.failed,
        _ => return None,
    })
}

fn seasons_label(r: &RequestDto) -> String {
    match (r.kind, r.seasons.as_slice()) {
        (MediaKind::Movie, _) => String::new(),
        (_, []) => "All seasons".into(),
        (_, [one]) => format!("Season {one}"),
        (_, many) => format!(
            "Seasons {}",
            many.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
        ),
    }
}

#[derive(Clone, Copy)]
enum Pending {
    Decline,
    Delete,
}

#[component]
pub fn RequestsPage() -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let can_manage = move || auth.has(Permissions::MANAGE_REQUESTS);
    let can_view_all = move || auth.has(Permissions::VIEW_REQUESTS);

    let status = RwSignal::new(String::from("all"));
    let kind = RwSignal::new(String::new());
    let user_id = RwSignal::new(String::new());
    let sort = RwSignal::new(String::from("created"));
    let selected = RwSignal::new(Vec::<i64>::new());
    let drawer_open = RwSignal::new(false);
    let drawer_item = RwSignal::new(None::<RequestDto>);

    // Decline (with reason) / delete targets: one or many ids.
    let action_ids = RwSignal::new(Vec::<i64>::new());
    let decline_open = RwSignal::new(false);
    let decline_reason = RwSignal::new(String::new());
    let delete_open = RwSignal::new(false);
    let edit_open = RwSignal::new(false);
    // Book / album / radio requests are listed by their own table.
    let content_mode = move || matches!(kind.get().as_str(), "media" | "book" | "music" | "radio");
    let has_content = move || {
        auth.has_any(
            Permissions::REQUEST_BOOK
                | Permissions::REQUEST_MUSIC
                | Permissions::REQUEST_RADIO
                | Permissions::VIEW_REQUESTS,
        )
    };

    let list = PagedList::new(
        PAGE_SIZE,
        |r: &RequestDto| r.id,
        move |page, size| {
            let skip = untrack(content_mode);
            let mut q = format!(
                "/requests?status={}&sort={}&page={page}&page_size={size}",
                status.get_untracked(),
                sort.get_untracked(),
            );
            let k = kind.get_untracked();
            if !k.is_empty() {
                q.push_str(&format!("&kind={k}"));
            }
            let u = user_id.get_untracked();
            if !u.is_empty() {
                q.push_str(&format!("&user_id={u}"));
            }
            async move {
                if skip {
                    return Ok((Vec::new(), 0));
                }
                api::get::<RequestPageDto>(&q).await.map(|p| (p.items, p.total))
            }
        },
    );
    // A new filter starts over from the first rows.
    Effect::new(move |_| {
        let _ = (status.get(), sort.get(), kind.get(), user_id.get());
        list.reset();
    });
    let counts = LocalResource::new(|| api::get::<RequestCountsDto>("/requests/count"));
    let users = LocalResource::new(move || {
        let want = auth.has(Permissions::MANAGE_USERS) && auth.has(Permissions::VIEW_REQUESTS);
        async move {
            if want {
                api::get::<Vec<UserDetailDto>>("/users").await.ok()
            } else {
                None
            }
        }
    });
    let refresh = move || {
        list.refresh();
        counts.refetch();
    };
    refetch_on_change(&["request"], refresh);

    // Keep the drawer showing fresh data after list reloads.
    Effect::new(move |_| {
        let fresh = list.items.with(|items| {
            let cur = drawer_item.get_untracked()?;
            items.iter().find(|r| r.id == cur.id && **r != cur).cloned()
        });
        if let Some(fresh) = fresh {
            drawer_item.set(Some(fresh));
        }
    });

    let clear_selection = move || selected.set(Vec::new());

    let approve = move |ids: Vec<i64>| {
        spawn_local(async move {
            let mut ok = 0;
            for id in &ids {
                match api::post_empty::<RequestDto>(&format!("/requests/{id}/approve")).await {
                    Ok(_) => ok += 1,
                    Err(e) => toasts.error(format!("Could not approve request #{id}: {e}")),
                }
            }
            if ok > 0 {
                toasts.success(if ok == 1 {
                    "Request approved.".to_string()
                } else {
                    format!("{ok} requests approved.")
                });
            }
            selected.set(Vec::new());
            refresh();
        });
    };
    let retry = move |id: i64| {
        spawn_local(async move {
            match api::post_empty::<RequestDto>(&format!("/requests/{id}/retry")).await {
                Ok(_) => toasts.success("Retrying the request."),
                Err(e) => toasts.error(format!("Retry failed: {e}")),
            }
            refresh();
        });
    };
    let ask_decline = move |ids: Vec<i64>| {
        action_ids.set(ids);
        decline_reason.set(String::new());
        decline_open.set(true);
    };
    let ask_delete = move |ids: Vec<i64>| {
        action_ids.set(ids);
        delete_open.set(true);
    };
    let run = move |what: Pending| {
        let ids = action_ids.get_untracked();
        let reason = decline_reason.get_untracked().trim().to_string();
        spawn_local(async move {
            let mut ok = 0;
            for id in &ids {
                let res = match what {
                    Pending::Decline => {
                        let body = DeclineRequestDto {
                            reason: (!reason.is_empty()).then(|| reason.clone()),
                        };
                        api::post::<_, RequestDto>(&format!("/requests/{id}/decline"), &body)
                            .await
                            .map(|_| ())
                    }
                    Pending::Delete => api::delete(&format!("/requests/{id}")).await,
                };
                match res {
                    Ok(()) => ok += 1,
                    Err(e) => toasts.error(format!("Request #{id}: {e}")),
                }
            }
            if ok > 0 {
                let verb = match what {
                    Pending::Decline => "declined",
                    Pending::Delete => "deleted",
                };
                toasts.success(if ok == 1 {
                    format!("Request {verb}.")
                } else {
                    format!("{ok} requests {verb}.")
                });
            }
            if matches!(what, Pending::Delete) {
                drawer_open.set(false);
            }
            selected.set(Vec::new());
            refresh();
        });
    };
    let confirm_delete = Callback::new(move |()| run(Pending::Delete));

    let tabs = move || {
        let c = counts.get().and_then(Result::ok);
        TABS.iter()
            .map(|(value, label)| {
                let n = c.as_ref().and_then(|c| tab_count(c, value));
                let v = *value;
                view! {
                    <button type="button" role="tab" class="tab" class:active=move || status.get() == v
                        aria-selected=move || (status.get() == v).to_string()
                        on:click=move |_| { status.set(v.to_string()); clear_selection(); }>
                        {*label}
                        {n.map(|n| view! { <span class="tab-count">{n}</span> })}
                    </button>
                }
            })
            .collect_view()
    };

    let user_filter = move || {
        let us = users.get().flatten()?;
        Some(view! {
            <select class="input input-sm" aria-label="Requested by" prop:value=move || user_id.get()
                on:change=move |e| { user_id.set(event_target_value(&e)); clear_selection(); }>
                <option value="">"All users"</option>
                {us.into_iter().map(|u| view! { <option value=u.id.to_string()>{u.display_name}</option> }).collect_view()}
            </select>
        })
    };

    let bulk_bar = move || {
        let ids = selected.get();
        (!ids.is_empty() && can_manage() && !content_mode()).then(|| {
            let (a, d, x) = (ids.clone(), ids.clone(), ids.clone());
            view! {
                <div class="bulk-bar" role="toolbar" aria-label="Bulk actions">
                    <span>{format!("{} selected", ids.len())}</span>
                    <button type="button" class="btn btn-sm btn-primary" on:click=move |_| approve(a.clone())>
                        <Icon name="check" /> "Approve"
                    </button>
                    <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| ask_decline(d.clone())>
                        <Icon name="close" /> "Decline"
                    </button>
                    <button type="button" class="btn btn-sm btn-ghost btn-danger-text" on:click=move |_| ask_delete(x.clone())>
                        <Icon name="trash" /> "Delete"
                    </button>
                    <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| selected.set(Vec::new())>"Clear"</button>
                </div>
            }
        })
    };

    let rows = move || {
        if !list.ready.get() {
            return view! { <Skeleton lines=6 /> }.into_any();
        }
        let items = list.items.get();
        if items.is_empty() {
            return match list.error.get() {
                Some(e) => {
                    view! { <ErrorBox error=e on_retry=Callback::new(move |()| list.reset()) /> }.into_any()
                }
                None => view! {
                    <EmptyState icon="requests" title="No requests"
                        description="Find something on Discover and press Request — it shows up here.">
                        <A href="/discover" attr:class="btn btn-primary">"Go to Discover"</A>
                    </EmptyState>
                }
                .into_any(),
            };
        }
        {
            let ids: Vec<i64> = items.iter().map(|r| r.id).collect();
            // Select-all covers the rows loaded so far.
            let all_label = format!("Select all {} loaded requests", ids.len());
            let all_ids = StoredValue::new(ids);
            let all_checked = move || {
                let sel = selected.get();
                all_ids.with_value(|ids| !ids.is_empty() && ids.iter().all(|i| sel.contains(i)))
            };
            let manage = can_manage();
            let show_user = can_view_all();
            view! {
                <div class="table-wrap">
                    <table class="table request-table">
                        <caption class="visually-hidden">"Requests"</caption>
                        <thead>
                            <tr>
                                {manage.then(|| view! {
                                    <th scope="col" class="cell-check">
                                        <input type="checkbox" aria-label=all_label.clone() title=all_label.clone() prop:checked=all_checked
                                            on:change=move |e| {
                                                if event_target_checked(&e) {
                                                    selected.set(all_ids.get_value());
                                                } else {
                                                    selected.set(Vec::new());
                                                }
                                            } />
                                    </th>
                                })}
                                <th scope="col">"Title"</th>
                                <th scope="col">"Status"</th>
                                {show_user.then(|| view! { <th scope="col">"Requested by"</th> })}
                                <th scope="col">"Requested"</th>
                                <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                            </tr>
                        </thead>
                        <tbody>
                            {items.into_iter().map(|r| {
                                let id = r.id;
                                let row = StoredValue::new(r.clone());
                                let thumb = tmdb_image(r.poster.as_deref(), "w92");
                                let pending = r.status == RequestStatus::Pending;
                                let failed = r.status == RequestStatus::Failed;
                                let is_own = auth.user.with_untracked(|u| u.as_ref().is_some_and(|u| u.id == r.requested_by.id));
                                let can_delete = manage || (is_own && pending);
                                let details_label = format!("Details for {}", r.title);
                                let sub = [Some(kind_label(r.kind).to_string()), r.year.map(|y| y.to_string()),
                                    Some(seasons_label(&r)).filter(|s| !s.is_empty())]
                                    .into_iter().flatten().collect::<Vec<_>>().join(" · ");
                                view! {
                                    <tr class="clickable" on:click=move |_| { drawer_item.set(Some(row.get_value())); drawer_open.set(true); }>
                                        {manage.then(|| view! {
                                            <td class="cell-check" on:click=|e| e.stop_propagation()>
                                                <input type="checkbox" aria-label=format!("Select {}", r.title)
                                                    prop:checked=move || selected.with(|s| s.contains(&id))
                                                    on:change=move |e| {
                                                        let on = event_target_checked(&e);
                                                        selected.update(|s| { s.retain(|x| *x != id); if on { s.push(id); } });
                                                    } />
                                            </td>
                                        })}
                                        <td>
                                            <div class="request-title-cell">
                                                <span class="rq-thumb">
                                                    {thumb.map(|src| view! { <img src=src alt="" loading="lazy" /> })}
                                                </span>
                                                <div>
                                                    <A href=media_href(r.kind, r.tmdb_id) attr:class="cell-strong" on:click=|e| e.stop_propagation()>
                                                        {r.title.clone()}
                                                    </A>
                                                    <div class="cell-sub">
                                                        {sub}
                                                        {r.is_4k.then(|| view! { <span class="badge tone-accent badge-xs">"4K"</span> })}
                                                        " · "
                                                        <button type="button" class="link-btn"
                                                            aria-label=details_label.clone()
                                                            on:click=move |e| {
                                                                e.stop_propagation();
                                                                drawer_item.set(Some(row.get_value()));
                                                                drawer_open.set(true);
                                                            }>
                                                            "Details"
                                                        </button>
                                                    </div>
                                                </div>
                                            </div>
                                        </td>
                                        <td>
                                            <RequestStatusBadge status=r.status />
                                            {r.decline_reason.clone().or(r.failure_reason.clone()).map(|why| view! { <div class="cell-sub">{why}</div> })}
                                        </td>
                                        {show_user.then(|| view! { <td><UserChip user=r.requested_by.clone() /></td> })}
                                        <td title=format::datetime(r.created_at)>{format::relative(r.created_at)}</td>
                                        <td class="cell-actions" on:click=|e| e.stop_propagation()>
                                            {(manage && pending).then(|| view! {
                                                <button type="button" class="btn btn-sm btn-primary" on:click=move |_| approve(vec![id])>
                                                    <Icon name="check" /> "Approve"
                                                </button>
                                                <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| ask_decline(vec![id])>
                                                    "Decline"
                                                </button>
                                            })}
                                            {(manage && failed).then(|| view! {
                                                <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| retry(id)>
                                                    <Icon name="refresh" /> "Retry"
                                                </button>
                                            })}
                                            {can_delete.then(|| view! {
                                                <button type="button" class="icon-btn icon-btn-sm" aria-label="Delete request"
                                                    title="Delete request" on:click=move |_| ask_delete(vec![id])>
                                                    <Icon name="trash" />
                                                </button>
                                            })}
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
                {move || list.error.get().map(|e| view! { <ErrorBox error=e on_retry=Callback::new(move |()| list.retry()) /> })}
            }
            .into_any()
        }
    };

    let drawer_title =
        Signal::derive(move || drawer_item.with(|r| r.as_ref().map(|r| r.title.clone()).unwrap_or_default()));
    let drawer_body = move || {
        drawer_item.get().map(|r| {
            let id = r.id;
            let backdrop = tmdb_image(r.backdrop.as_deref().or(r.poster.as_deref()), "w780");
            let pending = r.status == RequestStatus::Pending;
            let manage = can_manage();
            view! {
                {backdrop.map(|src| view! { <img class="drawer-art" src=src alt="" /> })}
                <div class="drawer-status">
                    <RequestStatusBadge status=r.status />
                    {r.is_4k.then(|| view! { <span class="badge tone-accent">"4K"</span> })}
                    <span class="badge tone-neutral">{kind_label(r.kind)}</span>
                </div>
                <dl class="kv">
                    <div class="kv-row"><dt>"Requested by"</dt><dd><UserChip user=r.requested_by.clone() /></dd></div>
                    <div class="kv-row"><dt>"Requested"</dt><dd>{format::datetime(r.created_at)}</dd></div>
                    {(r.kind == MediaKind::Series).then(|| view! {
                        <div class="kv-row"><dt>"Seasons"</dt><dd>{seasons_label(&r)}</dd></div>
                    })}
                    {r.modified_by.clone().map(|u| view! {
                        <div class="kv-row"><dt>{match r.status { RequestStatus::Declined => "Declined by", RequestStatus::Pending => "Edited by", _ => "Approved by" }}</dt>
                            <dd><UserChip user=u /></dd></div>
                    })}
                    {r.approved_at.map(|t| view! { <div class="kv-row"><dt>"Approved"</dt><dd>{format::datetime(t)}</dd></div> })}
                    {r.available_at.map(|t| view! { <div class="kv-row"><dt>"Available"</dt><dd>{format::datetime(t)}</dd></div> })}
                    {r.decline_reason.clone().map(|t| view! { <div class="kv-row"><dt>"Decline reason"</dt><dd>{t}</dd></div> })}
                    {r.failure_reason.clone().map(|t| view! { <div class="kv-row"><dt>"Failure"</dt><dd class="text-danger">{t}</dd></div> })}
                    {(r.profile_id.is_some() || r.root_folder_id.is_some()).then(|| view! {
                        <div class="kv-row"><dt>"Overrides"</dt><dd>
                            {r.profile_id.map(|p| format!("Profile #{p} ")).unwrap_or_default()}
                            {r.root_folder_id.map(|p| format!("Root folder #{p}")).unwrap_or_default()}
                        </dd></div>
                    })}
                    <div class="kv-row"><dt>"Last update"</dt><dd>{format!("{} ({})", request_status_label(r.status), format::relative(r.updated_at))}</dd></div>
                </dl>
                <div class="drawer-actions">
                    <A href=media_href(r.kind, r.tmdb_id) attr:class="btn btn-secondary">"Open title"</A>
                    {(auth.has(Permissions::MANAGE_LIBRARY)).then_some(()).and(r.movie_id.map(|m| format!("/movies/{m}")).or(r.series_id.map(|s| format!("/series/{s}"))))
                        .map(|h| view! { <A href=h attr:class="btn btn-ghost">"Open in library"</A> })}
                    {(manage && pending).then(|| view! {
                        <button type="button" class="btn btn-primary" on:click=move |_| approve(vec![id])><Icon name="check" />"Approve"</button>
                        <button type="button" class="btn btn-secondary" on:click=move |_| ask_decline(vec![id])>"Decline"</button>
                    })}
                    {{
                        let me = auth.user.with(|u| u.as_ref().map(|u| u.id));
                        let editable = pending && (manage || me == Some(r.requested_by.id));
                        editable.then(|| view! {
                            <button type="button" class="btn btn-secondary" on:click=move |_| edit_open.set(true)>
                                <Icon name="settings" />"Edit"
                            </button>
                        })
                    }}
                    {(manage && r.status == RequestStatus::Failed).then(|| view! {
                        <button type="button" class="btn btn-secondary" on:click=move |_| retry(id)><Icon name="refresh" />"Retry"</button>
                    })}
                </div>
            }
        })
    };

    view! {
        <PageHeader
            title="Requests"
            subtitle=if auth.has(Permissions::VIEW_REQUESTS) { "Everything your users have asked for." } else { "Titles you've asked for and where they are." }
        >
            <A href="/discover" attr:class="btn btn-primary"><Icon name="plus" />"New request"</A>
        </PageHeader>
        <div class="tabs" role="tablist" aria-label="Filter by status">{tabs}</div>
        <div class="toolbar">
            <select class="input input-sm" aria-label="Type" prop:value=move || kind.get()
                on:change=move |e| { kind.set(event_target_value(&e)); clear_selection(); }>
                <option value="">"Movies & series"</option>
                <option value="movie">"Movies"</option>
                <option value="series">"Series"</option>
                {move || has_content().then(|| view! {
                    <option value="media">"Books, music & radio"</option>
                    <option value="book">"Books"</option>
                    <option value="music">"Music"</option>
                    <option value="radio">"Radio"</option>

                })}
            </select>
            {user_filter}
            <select class="input input-sm" aria-label="Sort" prop:value=move || sort.get()
                on:change=move |e| { sort.set(event_target_value(&e)); clear_selection(); }>
                <option value="created">"Newest first"</option>
                <option value="updated">"Recently updated"</option>
                <option value="title">"Title"</option>
            </select>
        </div>
        {bulk_bar}
        {move || if content_mode() {
            view! {
                <content::ContentRequestTable status=status kind=kind sort=sort user_id=user_id
                    on_change=Callback::new(move |()| counts.refetch()) />
            }.into_any()
        } else {
            view! {
                <div class="card">
                    <div class="card-body flush">
                        {rows}
                        <InfiniteSentinel
                    on_visible=Callback::new(move |()| list.load_more())
                    enabled=Signal::derive(move || list.can_load_more())
                    loading=list.loading
                    loaded=Signal::derive(move || list.loaded())
                    total=list.total
                />
                    </div>
                </div>
            }
            .into_any()
        }}

        <Drawer open=drawer_open title=drawer_title>
            {drawer_body}
        </Drawer>
        <RequestEditModal
            open=edit_open
            request=drawer_item
            on_saved=Callback::new(move |r: RequestDto| {
                drawer_item.set(Some(r));
                refresh();
            })
        />
        <Modal open=decline_open title="Decline request">
            <form class="form" on:submit=move |e| { e.prevent_default(); decline_open.set(false); run(Pending::Decline); }>
                <p class="modal-text">{move || {
                    let n = action_ids.with(Vec::len);
                    if n == 1 { "The requester will see your reason.".to_string() } else { format!("Decline {n} requests. Requesters will see your reason.") }
                }}</p>
                <div class="field">
                    <label for="decline-reason">"Reason " <span class="optional">"(optional)"</span></label>
                    <textarea id="decline-reason" class="input textarea" rows="3" placeholder="e.g. Already available on another server"
                        prop:value=move || decline_reason.get() on:input=move |e| decline_reason.set(event_target_value(&e))></textarea>
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| decline_open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-danger">"Decline"</button>
                </div>
            </form>
        </Modal>
        <ConfirmDialog
            open=delete_open
            title="Delete request"
            message=Signal::derive(move || {
                let n = action_ids.with(Vec::len);
                if n == 1 { "Delete this request? Titles already added to the library stay there.".to_string() }
                else { format!("Delete {n} requests? Titles already added to the library stay there.") }
            })
            confirm_label="Delete"
            on_confirm=confirm_delete
        />
    }
}

/// Edits a pending request: seasons (series), 4K and, for advanced requesters / managers,
/// the quality profile and root folder (`PUT /requests/{id}`).
#[component]
fn RequestEditModal(
    open: RwSignal<bool>,
    request: RwSignal<Option<RequestDto>>,
    on_saved: Callback<RequestDto>,
) -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let seasons = RwSignal::new(Vec::<u32>::new());
    let is_4k = RwSignal::new(false);
    let profile = RwSignal::new(String::new());
    let root = RwSignal::new(String::new());
    let saving = RwSignal::new(false);
    let advanced = move || auth.has(Permissions::REQUEST_ADVANCED) || auth.has(Permissions::MANAGE_REQUESTS);
    let can_4k = move || auth.has(Permissions::REQUEST_4K);

    // Reset the form whenever the dialog opens.
    Effect::new(move |_| {
        if open.get()
            && let Some(r) = request.get_untracked()
        {
            seasons.set(r.seasons.clone());
            is_4k.set(r.is_4k);
            profile.set(r.profile_id.map(|p| p.to_string()).unwrap_or_default());
            root.set(r.root_folder_id.map(|p| p.to_string()).unwrap_or_default());
        }
    });

    let detail = LocalResource::new(move || {
        let target = open
            .get()
            .then(|| request.with_untracked(|r| r.as_ref().map(|r| (r.kind, r.tmdb_id))))
            .flatten();
        async move {
            match target {
                Some((MediaKind::Series, id)) => api::get::<MediaDetailDto>(&format!("/media/series/{id}"))
                    .await
                    .ok(),
                _ => None,
            }
        }
    });
    let options = LocalResource::new(move || {
        let target = (open.get() && advanced())
            .then(|| request.with_untracked(|r| r.as_ref().map(|r| r.kind)))
            .flatten();
        let four_k = is_4k.get();
        async move {
            let kind = match target? {
                MediaKind::Movie => "movie",
                MediaKind::Series => "series",
            };
            api::get::<RequestOptionsDto>(&format!("/requests/options?kind={kind}&is_4k={four_k}"))
                .await
                .ok()
        }
    });

    let is_series = move || request.with(|r| r.as_ref().is_some_and(|r| r.kind == MediaKind::Series));
    let season_picker = move || {
        if !is_series() {
            return None;
        }
        let list = detail.get().flatten().map(|d| d.seasons).unwrap_or_default();
        Some(if list.is_empty() {
            view! { <p class="muted">"Loading seasons…"</p> }.into_any()
        } else {
            view! {
                <fieldset class="field">
                    <legend>"Seasons"</legend>
                    <div class="season-picks">
                        {list
                            .into_iter()
                            .filter(|s| s.season_number > 0)
                            .map(|s| {
                                let n = s.season_number;
                                let label = s.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("Season {n}"));
                                view! {
                                    <label class="checkbox">
                                        <input type="checkbox" prop:checked=move || seasons.with(|v| v.contains(&n))
                                            on:change=move |e| {
                                                let on = event_target_checked(&e);
                                                seasons.update(|v| {
                                                    v.retain(|x| *x != n);
                                                    if on {
                                                        v.push(n);
                                                        v.sort_unstable();
                                                    }
                                                });
                                            } />
                                        {label}
                                    </label>
                                }
                            })
                            .collect_view()}
                    </div>
                </fieldset>
            }
            .into_any()
        })
    };
    let advanced_fields = move || {
        if !advanced() {
            return None;
        }
        let o = options.get().flatten();
        Some(view! {
            <div class="field-row">
                <div class="field">
                    <label for="req-edit-profile">"Quality profile"</label>
                    <select id="req-edit-profile" class="input" prop:value=move || profile.get()
                        on:change=move |e| profile.set(event_target_value(&e))>
                        <option value="">"Default"</option>
                        {o.as_ref().map(|o| o.profiles.iter().map(|p| {
                            let v = p.id.to_string();
                            view! { <option value=v.clone() selected=move || profile.get() == v>{p.name.clone()}</option> }
                        }).collect_view())}
                    </select>
                </div>
                <div class="field">
                    <label for="req-edit-root">"Root folder"</label>
                    <select id="req-edit-root" class="input" prop:value=move || root.get()
                        on:change=move |e| root.set(event_target_value(&e))>
                        <option value="">"Default"</option>
                        {o.as_ref().map(|o| o.root_folders.iter().map(|r| {
                            let v = r.id.to_string();
                            view! { <option value=v.clone() selected=move || root.get() == v>{r.path.clone()}</option> }
                        }).collect_view())}
                    </select>
                </div>
            </div>
        })
    };

    let save = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let Some(r) = request.get_untracked() else {
            return;
        };
        if r.kind == MediaKind::Series && seasons.with_untracked(Vec::is_empty) {
            toasts.error("Pick at least one season.");
            return;
        }
        let body = UpdateRequestDto {
            seasons: (r.kind == MediaKind::Series).then(|| seasons.get_untracked()),
            is_4k: can_4k().then(|| is_4k.get_untracked()),
            profile_id: if advanced() {
                profile.get_untracked().parse().ok()
            } else {
                None
            },
            root_folder_id: if advanced() {
                root.get_untracked().parse().ok()
            } else {
                None
            },
            tags: None,
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, RequestDto>(&format!("/requests/{}", r.id), &body).await {
                Ok(updated) => {
                    toasts.success("Request updated.");
                    open.set(false);
                    on_saved.run(updated);
                }
                Err(e) => toasts.error(format!("Could not update the request: {e}")),
            }
            saving.set(false);
        });
    };

    view! {
        <Modal open=open title="Edit request">
            <form class="form" on:submit=save>
                <p class="modal-text">{move || request.with(|r| r.as_ref().map(|r| r.title.clone()).unwrap_or_default())}</p>
                {season_picker}
                {move || can_4k().then(|| view! {
                    <label class="checkbox">
                        <input type="checkbox" prop:checked=move || is_4k.get() on:change=move |e| is_4k.set(event_target_checked(&e)) />
                        "Request in 4K"
                    </label>
                })}
                {advanced_fields}
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                </div>
            </form>
        </Modal>
    }
}
