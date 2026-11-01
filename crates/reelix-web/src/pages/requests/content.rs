//! Book and album requests on `/requests` (`GET /requests/media`): author/artist, formats,
//! approve / decline / retry / delete and a format editor for pending book requests.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::Permissions;
use reelix_api_types::requests::{
    ContentRequestDto, ContentRequestKind, ContentRequestPageDto, DeclineRequestDto, RequestStatus,
    UpdateContentRequestDto,
};
use reelix_core::BookFormat;

use super::{PAGE_SIZE, Pager};
use crate::api;
use crate::components::media_card::tmdb_image;
use crate::components::request_drawer::Drawer;
use crate::components::request_status::{
    RequestStatusBadge, UserChip, refetch_on_change, request_status_label,
};
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, Modal, Skeleton};
use crate::format;
use crate::pages::discover::content::{content_kind_label, format_label, formats_label};
use crate::state::{AuthState, Toasts};

/// Library page of an approved book / album request.
fn library_href(r: &ContentRequestDto) -> Option<String> {
    match r.kind {
        ContentRequestKind::Book => r.author_id.map(|a| format!("/books/authors/{a}")),
        ContentRequestKind::Music => r.artist_id.map(|a| format!("/music/artists/{a}")),
    }
}

fn subline(r: &ContentRequestDto) -> String {
    [
        Some(content_kind_label(r.kind).to_string()),
        r.subtitle.clone(),
        r.year.map(|y| y.to_string()),
        Some(formats_label(&r.formats)).filter(|f| !f.is_empty()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

#[derive(Clone, Copy)]
enum Pending {
    Decline,
    Delete,
}

/// The book/album request table. `kind`: `media` (both), `book` or `music`.
#[component]
pub fn ContentRequestTable(
    status: RwSignal<String>,
    kind: RwSignal<String>,
    sort: RwSignal<String>,
    user_id: RwSignal<String>,
    page: RwSignal<u32>,
    /// Called after any change (to refresh the status counts).
    on_change: Callback<()>,
) -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let can_manage = move || auth.has(Permissions::MANAGE_REQUESTS);
    let show_user = move || auth.has(Permissions::VIEW_REQUESTS);
    let drawer_open = RwSignal::new(false);
    let drawer_item = RwSignal::new(None::<ContentRequestDto>);
    let action_id = RwSignal::new(0i64);
    let decline_open = RwSignal::new(false);
    let decline_reason = RwSignal::new(String::new());
    let delete_open = RwSignal::new(false);
    let edit_open = RwSignal::new(false);

    let list = LocalResource::new(move || {
        let k = match kind.get().as_str() {
            "book" => "book",
            "music" => "music",
            _ => "all",
        };
        let mut q = format!(
            "/requests/media?kind={k}&status={}&sort={}&page={}&page_size={PAGE_SIZE}",
            status.get(),
            sort.get(),
            page.get()
        );
        let u = user_id.get();
        if !u.is_empty() {
            q.push_str(&format!("&user_id={u}"));
        }
        async move { api::get::<ContentRequestPageDto>(&q).await }
    });
    let refresh = move || {
        list.refetch();
        on_change.run(());
    };
    refetch_on_change(&["request"], move || list.refetch());
    Effect::new(move |_| {
        if let Some(Ok(p)) = list.get()
            && let Some(cur) = drawer_item.get_untracked()
            && let Some(fresh) = p.items.iter().find(|r| r.id == cur.id)
        {
            drawer_item.set(Some(fresh.clone()));
        }
    });

    let act = move |id: i64, verb: &'static str| {
        spawn_local(async move {
            match api::post_empty::<ContentRequestDto>(&format!("/requests/media/{id}/{verb}")).await {
                Ok(r) if r.status == RequestStatus::Failed => toasts.error(format!(
                    "Adding “{}” failed: {}",
                    r.title,
                    r.failure_reason.unwrap_or_default()
                )),
                Ok(_) => toasts.success(if verb == "approve" {
                    "Request approved."
                } else {
                    "Retrying the request."
                }),
                Err(e) => toasts.error(format!("Request #{id}: {e}")),
            }
            refresh();
        });
    };
    let ask = move |id: i64, what: Pending| {
        action_id.set(id);
        match what {
            Pending::Decline => {
                decline_reason.set(String::new());
                decline_open.set(true);
            }
            Pending::Delete => delete_open.set(true),
        }
    };
    let run = move |what: Pending| {
        let id = action_id.get_untracked();
        let reason = decline_reason.get_untracked().trim().to_string();
        spawn_local(async move {
            let res = match what {
                Pending::Decline => {
                    let body = DeclineRequestDto {
                        reason: (!reason.is_empty()).then_some(reason),
                    };
                    api::post::<_, ContentRequestDto>(&format!("/requests/media/{id}/decline"), &body)
                        .await
                        .map(|_| ())
                }
                Pending::Delete => api::delete(&format!("/requests/media/{id}")).await,
            };
            match res {
                Ok(()) => {
                    toasts.success(match what {
                        Pending::Decline => "Request declined.",
                        Pending::Delete => "Request deleted.",
                    });
                    if matches!(what, Pending::Delete) {
                        drawer_open.set(false);
                    }
                }
                Err(e) => toasts.error(format!("Request #{id}: {e}")),
            }
            refresh();
        });
    };

    let rows = move || match list.get() {
        None => view! { <Skeleton lines=6 /> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| list.refetch()) /> }.into_any()
        }
        Some(Ok(p)) if p.items.is_empty() => view! {
            <EmptyState icon="requests" title="No book or album requests"
                description="Find books and albums on Discover and press Request — they show up here." />
        }
        .into_any(),
        Some(Ok(p)) => {
            let manage = can_manage();
            let users = show_user();
            view! {
                <div class="table-wrap">
                    <table class="table request-table">
                        <caption class="visually-hidden">"Book and album requests"</caption>
                        <thead>
                            <tr>
                                <th scope="col">"Title"</th>
                                <th scope="col">"Status"</th>
                                {users.then(|| view! { <th scope="col">"Requested by"</th> })}
                                <th scope="col">"Requested"</th>
                                <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                            </tr>
                        </thead>
                        <tbody>
                            {p.items.into_iter().map(|r| {
                                let id = r.id;
                                let row = StoredValue::new(r.clone());
                                let thumb = tmdb_image(r.poster.as_deref(), "w92");
                                let pending = r.status == RequestStatus::Pending;
                                let failed = r.status == RequestStatus::Failed;
                                let own = auth.user.with_untracked(|u| u.as_ref().is_some_and(|u| u.id == r.requested_by.id));
                                let can_delete = manage || (own && matches!(r.status, RequestStatus::Pending | RequestStatus::Declined | RequestStatus::Failed));
                                view! {
                                    <tr class="clickable" on:click=move |_| { drawer_item.set(Some(row.get_value())); drawer_open.set(true); }>
                                        <td>
                                            <div class="request-title-cell">
                                                <span class="rq-thumb">
                                                    {thumb.map(|src| view! { <img src=src alt="" loading="lazy" /> })}
                                                </span>
                                                <div>
                                                    // A real button so keyboard users can open the details too.
                                                    <button type="button" class="link-btn cell-strong"
                                                        on:click=move |e| {
                                                            e.stop_propagation();
                                                            drawer_item.set(Some(row.get_value()));
                                                            drawer_open.set(true);
                                                        }>
                                                        {r.title.clone()}
                                                    </button>
                                                    <div class="cell-sub">{subline(&r)}</div>
                                                </div>
                                            </div>
                                        </td>
                                        <td>
                                            <RequestStatusBadge status=r.status />
                                            {r.decline_reason.clone().or(r.failure_reason.clone()).map(|why| view! { <div class="cell-sub">{why}</div> })}
                                        </td>
                                        {users.then(|| view! { <td><UserChip user=r.requested_by.clone() /></td> })}
                                        <td title=format::datetime(r.created_at)>{format::relative(r.created_at)}</td>
                                        <td class="cell-actions" on:click=|e| e.stop_propagation()>
                                            {(manage && pending).then(|| view! {
                                                <button type="button" class="btn btn-sm btn-primary" on:click=move |_| act(id, "approve")>
                                                    <Icon name="check" /> "Approve"
                                                </button>
                                                <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| ask(id, Pending::Decline)>
                                                    "Decline"
                                                </button>
                                            })}
                                            {(manage && failed).then(|| view! {
                                                <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| act(id, "retry")>
                                                    <Icon name="refresh" /> "Retry"
                                                </button>
                                            })}
                                            {can_delete.then(|| view! {
                                                <button type="button" class="icon-btn icon-btn-sm" aria-label="Delete request"
                                                    title="Delete request" on:click=move |_| ask(id, Pending::Delete)>
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
                <Pager total=p.total page=page />
            }
            .into_any()
        }
    };

    let drawer_title =
        Signal::derive(move || drawer_item.with(|r| r.as_ref().map(|r| r.title.clone()).unwrap_or_default()));
    let drawer_body = move || {
        drawer_item.get().map(|r| {
            let id = r.id;
            let art = tmdb_image(r.poster.as_deref(), "w342");
            let pending = r.status == RequestStatus::Pending;
            let manage = can_manage();
            let me = auth.user.with(|u| u.as_ref().map(|u| u.id));
            let editable = pending && r.kind == ContentRequestKind::Book && (manage || me == Some(r.requested_by.id));
            let lib = auth.has(Permissions::MANAGE_LIBRARY).then(|| library_href(&r)).flatten();
            view! {
                {art.map(|src| view! { <img class="drawer-art" src=src alt="" /> })}
                <div class="drawer-status">
                    <RequestStatusBadge status=r.status />
                    <span class="badge tone-neutral">{content_kind_label(r.kind)}</span>
                    {r.formats.iter().map(|f| view! { <span class="badge tone-accent">{format_label(*f)}</span> }).collect_view()}
                </div>
                <dl class="kv">
                    {r.subtitle.clone().map(|s| view! {
                        <div class="kv-row"><dt>{match r.kind { ContentRequestKind::Book => "Author", ContentRequestKind::Music => "Artist" }}</dt><dd>{s}</dd></div>
                    })}
                    {r.year.map(|y| view! { <div class="kv-row"><dt>"Year"</dt><dd>{y}</dd></div> })}
                    <div class="kv-row"><dt>"Requested by"</dt><dd><UserChip user=r.requested_by.clone() /></dd></div>
                    <div class="kv-row"><dt>"Requested"</dt><dd>{format::datetime(r.created_at)}</dd></div>
                    {r.modified_by.clone().map(|u| view! {
                        <div class="kv-row"><dt>{match r.status { RequestStatus::Declined => "Declined by", RequestStatus::Pending => "Edited by", _ => "Approved by" }}</dt>
                            <dd><UserChip user=u /></dd></div>
                    })}
                    {r.approved_at.map(|t| view! { <div class="kv-row"><dt>"Approved"</dt><dd>{format::datetime(t)}</dd></div> })}
                    {r.available_at.map(|t| view! { <div class="kv-row"><dt>"Available"</dt><dd>{format::datetime(t)}</dd></div> })}
                    {r.decline_reason.clone().map(|t| view! { <div class="kv-row"><dt>"Decline reason"</dt><dd>{t}</dd></div> })}
                    {r.failure_reason.clone().map(|t| view! { <div class="kv-row"><dt>"Failure"</dt><dd class="text-danger">{t}</dd></div> })}
                    <div class="kv-row"><dt>"Last update"</dt><dd>{format!("{} ({})", request_status_label(r.status), format::relative(r.updated_at))}</dd></div>
                </dl>
                <div class="drawer-actions">
                    {lib.map(|h| view! { <A href=h attr:class="btn btn-ghost">"Open in library"</A> })}
                    {(manage && pending).then(|| view! {
                        <button type="button" class="btn btn-primary" on:click=move |_| act(id, "approve")><Icon name="check" />"Approve"</button>
                        <button type="button" class="btn btn-secondary" on:click=move |_| ask(id, Pending::Decline)>"Decline"</button>
                    })}
                    {editable.then(|| view! {
                        <button type="button" class="btn btn-secondary" on:click=move |_| edit_open.set(true)>
                            <Icon name="settings" />"Edit formats"
                        </button>
                    })}
                    {(manage && r.status == RequestStatus::Failed).then(|| view! {
                        <button type="button" class="btn btn-secondary" on:click=move |_| act(id, "retry")><Icon name="refresh" />"Retry"</button>
                    })}
                </div>
            }
        })
    };

    view! {
        <div class="card"><div class="card-body flush">{rows}</div></div>
        <Drawer open=drawer_open title=drawer_title>
            {drawer_body}
        </Drawer>
        <FormatEditModal open=edit_open request=drawer_item
            on_saved=Callback::new(move |r: ContentRequestDto| { drawer_item.set(Some(r)); refresh(); }) />
        <Modal open=decline_open title="Decline request">
            <form class="form" on:submit=move |e| { e.prevent_default(); decline_open.set(false); run(Pending::Decline); }>
                <p class="modal-text">"The requester will see your reason."</p>
                <div class="field">
                    <label for="cdecline-reason">"Reason " <span class="optional">"(optional)"</span></label>
                    <textarea id="cdecline-reason" class="input textarea" rows="3"
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
            message=Signal::derive(|| "Delete this request? Books and albums already added to the library stay there.".to_string())
            confirm_label="Delete"
            on_confirm=Callback::new(move |()| run(Pending::Delete))
        />
    }
}

/// Changes the formats of a pending book request (`PUT /requests/media/{id}`).
#[component]
fn FormatEditModal(
    open: RwSignal<bool>,
    request: RwSignal<Option<ContentRequestDto>>,
    on_saved: Callback<ContentRequestDto>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let formats = RwSignal::new(Vec::<BookFormat>::new());
    let saving = RwSignal::new(false);
    Effect::new(move |_| {
        if open.get()
            && let Some(r) = request.get_untracked()
        {
            formats.set(r.formats.clone());
        }
    });
    let save = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let Some(r) = request.get_untracked() else { return };
        let f = formats.get_untracked();
        if f.is_empty() {
            toasts.error("Pick at least one format.");
            return;
        }
        let body = UpdateContentRequestDto {
            formats: Some(f),
            ..Default::default()
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, ContentRequestDto>(&format!("/requests/media/{}", r.id), &body).await {
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
                <fieldset class="field">
                    <legend>"Formats"</legend>
                    {BookFormat::ALL.into_iter().map(|f| view! {
                        <label class="checkbox">
                            <input type="checkbox" prop:checked=move || formats.with(|v| v.contains(&f))
                                on:change=move |e| {
                                    let on = event_target_checked(&e);
                                    formats.update(|v| { v.retain(|x| *x != f); if on { v.push(f); v.sort(); } });
                                } />
                            {format_label(f)}
                        </label>
                    }).collect_view()}
                </fieldset>
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
