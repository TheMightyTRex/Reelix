//! `/issues` (list) and `/issues/{id}` (thread with comments, resolve / reopen).

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use reelix_api_types::Permissions;
use reelix_api_types::requests::{
    CreateCommentDto, IssueCountsDto, IssueDto, IssuePageDto, IssueStatus, UserBriefDto,
};

use crate::api;
use crate::components::media_card::tmdb_image;
use crate::components::request_status::{
    ISSUE_TYPES, UserChip, issue_status_tone, issue_type_label, kind_label, media_href, refetch_on_change,
};
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge};
use crate::format;
use crate::pages::requests::Pager;
use crate::state::{AuthState, Toasts};

const PAGE_SIZE: u32 = 25;

fn status_label(s: IssueStatus) -> &'static str {
    match s {
        IssueStatus::Open => "Open",
        IssueStatus::Resolved => "Resolved",
    }
}

fn scope_label(i: &IssueDto) -> Option<String> {
    match (i.season, i.episode) {
        (Some(s), Some(e)) => Some(format!("S{s:02}E{e:02}")),
        (Some(s), None) => Some(format!("Season {s}")),
        _ => None,
    }
}

#[component]
pub fn IssuesPage() -> impl IntoView {
    let status = RwSignal::new(String::from("open"));
    let issue_type = RwSignal::new(String::new());
    let page = RwSignal::new(1u32);

    let list = LocalResource::new(move || {
        let mut q = format!(
            "/issues?status={}&page={}&page_size={PAGE_SIZE}",
            status.get(),
            page.get()
        );
        let t = issue_type.get();
        if !t.is_empty() {
            q.push_str(&format!("&issue_type={t}"));
        }
        async move { api::get::<IssuePageDto>(&q).await }
    });
    let counts = LocalResource::new(|| api::get::<IssueCountsDto>("/issues/count"));
    refetch_on_change(&["issue"], move || {
        list.refetch();
        counts.refetch();
    });

    let tabs = move || {
        let c = counts.get().and_then(Result::ok).unwrap_or_default();
        [
            ("open", "Open", c.open),
            ("resolved", "Resolved", c.resolved),
            ("all", "All", c.total),
        ]
        .into_iter()
        .map(|(v, label, n)| {
            view! {
                <button type="button" role="tab" class="tab" class:active=move || status.get() == v
                    aria-selected=move || (status.get() == v).to_string()
                    on:click=move |_| { status.set(v.to_string()); page.set(1); }>
                    {label}
                    <span class="tab-count">{n}</span>
                </button>
            }
        })
        .collect_view()
    };

    let body = move || {
        match list.get() {
        None => view! { <Skeleton lines=6 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| list.refetch()) /> }.into_any(),
        Some(Ok(p)) if p.items.is_empty() => view! {
            <EmptyState icon="issues" title="No issues"
                description="Problems with playback, audio or subtitles can be reported from a title's page." />
        }
        .into_any(),
        Some(Ok(p)) => view! {
            <ul class="issue-list">
                {p.items.into_iter().map(|i| {
                    let thumb = tmdb_image(i.poster.as_deref(), "w92");
                    let sub = [Some(kind_label(i.kind).to_string()), i.year.map(|y| y.to_string()), scope_label(&i)]
                        .into_iter().flatten().collect::<Vec<_>>().join(" · ");
                    view! {
                        <li>
                            <A href=format!("/issues/{}", i.id) attr:class="issue-row">
                                <span class="rq-thumb">{thumb.map(|src| view! { <img src=src alt="" loading="lazy" /> })}</span>
                                <div class="issue-main">
                                    <div class="issue-title">
                                        <strong>{i.title.clone()}</strong>
                                        <span class="badge tone-info">{issue_type_label(i.issue_type)}</span>
                                        <StatusBadge tone=issue_status_tone(i.status) label=status_label(i.status) dot=true />
                                    </div>
                                    <p class="issue-snippet">{i.message.clone()}</p>
                                    <div class="cell-sub">
                                        {sub} " · " <UserChip user=i.reported_by.clone() /> " · " {format::relative(i.created_at)}
                                    </div>
                                </div>
                                <span class="issue-comments" title="Comments">
                                    <Icon name="list" />
                                    {i.comment_count}
                                </span>
                            </A>
                        </li>
                    }
                }).collect_view()}
            </ul>
            <Pager total=p.total page=page />
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="Issues" subtitle="Playback, audio and subtitle problems reported by users." />
        <div class="tabs" role="tablist" aria-label="Filter by status">{tabs}</div>
        <div class="toolbar">
            <select class="input input-sm" aria-label="Issue type" prop:value=move || issue_type.get()
                on:change=move |e| { issue_type.set(event_target_value(&e)); page.set(1); }>
                <option value="">"All types"</option>
                {ISSUE_TYPES.into_iter().map(|t| view! { <option value=t.as_str()>{issue_type_label(t)}</option> }).collect_view()}
            </select>
        </div>
        <div class="card"><div class="card-body">{body}</div></div>
    }
}

#[component]
fn Comment(
    user: UserBriefDto,
    message: String,
    at: chrono::DateTime<chrono::Utc>,
    #[prop(optional)] first: bool,
) -> impl IntoView {
    view! {
        <li class="comment" class:first=first>
            <div class="comment-head">
                <UserChip user=user />
                <span class="muted" title=format::datetime(at)>{format::relative(at)}</span>
                {first.then(|| view! { <span class="badge tone-neutral">"Reporter"</span> })}
            </div>
            <p class="comment-body">{message}</p>
        </li>
    }
}

#[component]
pub fn IssueDetailPage() -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let params = use_params_map();
    let id =
        Memo::new(move |_| params.with(|p| p.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0)));
    let issue = LocalResource::new(move || {
        let id = id.get();
        async move { api::get::<IssueDto>(&format!("/issues/{id}")).await }
    });
    refetch_on_change(&["issue"], move || issue.refetch());

    let comment = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let delete_open = RwSignal::new(false);

    let post_comment = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let msg = comment.get_untracked().trim().to_string();
        if msg.is_empty() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, IssueDto>(
                &format!("/issues/{}/comments", id.get_untracked()),
                &CreateCommentDto { message: msg },
            )
            .await
            {
                Ok(_) => {
                    comment.set(String::new());
                    issue.refetch();
                }
                Err(e) => toasts.error(format!("Could not post the comment: {e}")),
            }
            busy.set(false);
        });
    };
    let set_state = move |verb: &'static str| {
        spawn_local(async move {
            match api::post_empty::<IssueDto>(&format!("/issues/{}/{verb}", id.get_untracked())).await {
                Ok(_) => {
                    toasts.success(if verb == "resolve" {
                        "Issue resolved."
                    } else {
                        "Issue reopened."
                    });
                    issue.refetch();
                }
                Err(e) => toasts.error(format!("Could not update the issue: {e}")),
            }
        });
    };
    let confirm_delete = Callback::new(move |()| {
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::delete(&format!("/issues/{}", id.get_untracked())).await {
                Ok(()) => {
                    toasts.success("Issue deleted.");
                    navigate("/issues", Default::default());
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    let content = move || match issue.get() {
        None => view! { <Skeleton lines=6 /> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| issue.refetch()) /> }.into_any()
        }
        Some(Ok(i)) => {
            let manage = auth.has(Permissions::MANAGE_ISSUES);
            let is_reporter = auth
                .user
                .with_untracked(|u| u.as_ref().is_some_and(|u| u.id == i.reported_by.id));
            let open = i.status == IssueStatus::Open;
            let thumb = tmdb_image(i.poster.as_deref(), "w185");
            let sub = [
                Some(kind_label(i.kind).to_string()),
                i.year.map(|y| y.to_string()),
                scope_label(&i),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            view! {
                <div class="page-header">
                    <div>
                        <h1 class="page-title">{format!("{} issue · {}", issue_type_label(i.issue_type), i.title)}</h1>
                        <p class="page-subtitle">{format!("#{} opened {} by {}", i.id, format::relative(i.created_at), i.reported_by.display_name)}</p>
                    </div>
                    <div class="page-actions">
                        <StatusBadge tone=issue_status_tone(i.status) label=status_label(i.status) dot=true />
                        {(manage || is_reporter).then(|| if open {
                            view! { <button type="button" class="btn btn-primary btn-sm" on:click=move |_| set_state("resolve")><Icon name="check" />"Resolve"</button> }.into_any()
                        } else {
                            view! { <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| set_state("reopen")><Icon name="refresh" />"Reopen"</button> }.into_any()
                        })}
                        {manage.then(|| view! {
                            <button type="button" class="btn btn-ghost btn-sm btn-danger-text" on:click=move |_| delete_open.set(true)><Icon name="trash" />"Delete"</button>
                        })}
                    </div>
                </div>
                <div class="md-grid">
                    <section class="card">
                        <header class="card-header"><h2 class="card-title">"Discussion"</h2></header>
                        <div class="card-body">
                            <ul class="comment-thread">
                                <Comment user=i.reported_by.clone() message=i.message.clone() at=i.created_at first=true />
                                {i.comments.iter().cloned().map(|c| view! { <Comment user=c.user message=c.message at=c.created_at /> }).collect_view()}
                            </ul>
                            {i.resolved_by.clone().map(|u| view! {
                                <p class="thread-note"><Icon name="check" />{format!("Resolved by {}", u.display_name)}
                                    {i.resolved_at.map(|t| format!(" · {}", format::relative(t)))}</p>
                            })}
                            <form class="form comment-form" on:submit=post_comment>
                                <label class="visually-hidden" for="comment-input">"Add a comment"</label>
                                <textarea id="comment-input" class="input textarea" rows="3" placeholder="Add a comment…"
                                    prop:value=move || comment.get() on:input=move |e| comment.set(event_target_value(&e))></textarea>
                                <div class="form-actions">
                                    <button type="submit" class="btn btn-primary" disabled=move || busy.get() || comment.with(|c| c.trim().is_empty())>
                                        {move || if busy.get() { "Posting…" } else { "Comment" }}
                                    </button>
                                </div>
                            </form>
                        </div>
                    </section>
                    <aside class="card">
                        <div class="card-body issue-media">
                            <span class="rq-thumb rq-thumb-lg">{thumb.map(|src| view! { <img src=src alt="" /> })}</span>
                            <div>
                                <A href=media_href(i.kind, i.tmdb_id) attr:class="cell-strong">{i.title.clone()}</A>
                                <div class="cell-sub">{sub}</div>
                            </div>
                        </div>
                    </aside>
                </div>
            }
            .into_any()
        }
    };

    view! {
        <div class="back-link"><A href="/issues">"← All issues"</A></div>
        {content}
        <ConfirmDialog open=delete_open title="Delete issue" message=Signal::derive(|| "Delete this issue and its comments?".to_string())
            confirm_label="Delete" on_confirm=confirm_delete />
    }
}
