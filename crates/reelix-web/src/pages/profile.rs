//! `/profile` — the signed-in user's account, request quota, notification preferences and API keys.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::Permissions;
use reelix_api_types::requests::{NotificationPrefsDto, QuotaDto, QuotaUsageDto, USER_NOTIFICATION_EVENTS};

use crate::api;
use crate::components::request_modal::quota_line;
use crate::components::{ApiKeysPanel, Card, PageHeader, Skeleton, StatusBadge, Tone};
use crate::format;
use crate::state::auth::role_name;
use crate::state::{AuthState, Toasts};

const PERMISSION_LABELS: &[(Permissions, &str)] = &[
    (Permissions::ADMIN, "Administrator"),
    (Permissions::MANAGE_LIBRARY, "Manage library"),
    (Permissions::MANAGE_REQUESTS, "Manage requests"),
    (Permissions::MANAGE_USERS, "Manage users"),
    (Permissions::MANAGE_ISSUES, "Manage issues"),
    (Permissions::REQUEST_MOVIE, "Request movies"),
    (Permissions::REQUEST_TV, "Request series"),
    (Permissions::REQUEST_4K, "Request 4K"),
    (Permissions::REQUEST_ADVANCED, "Advanced requests"),
    (Permissions::AUTO_APPROVE_MOVIE, "Auto-approve movies"),
    (Permissions::AUTO_APPROVE_TV, "Auto-approve series"),
    (Permissions::AUTO_APPROVE_4K, "Auto-approve 4K"),
    (Permissions::VIEW_REQUESTS, "View all requests"),
    (Permissions::CREATE_ISSUES, "Report issues"),
    (Permissions::VIEW_CALENDAR, "View calendar"),
    (Permissions::VIEW_QUEUE, "View queue"),
];

#[component]
pub fn ProfilePage() -> impl IntoView {
    let auth = AuthState::expect();

    let account = move || {
        auth.user.get().map(|u| {
            let initial = u.display_name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            let perms: Vec<&str> = if u.permissions.is_admin() {
                vec!["Administrator (all permissions)"]
            } else {
                PERMISSION_LABELS.iter().filter(|(p, _)| u.permissions.contains(*p)).map(|(_, l)| *l).collect()
            };
            view! {
                <div class="profile-head">
                    <span class="avatar avatar-lg" aria-hidden="true">{initial}</span>
                    <div>
                        <h2 class="profile-name">{u.display_name.clone()}</h2>
                        <p class="muted">"@" {u.username.clone()}</p>
                    </div>
                    <StatusBadge tone=Tone::Accent label=role_name(u.permissions) />
                </div>
                <dl class="kv">
                    <div class="kv-row"><dt>"Email"</dt><dd>{u.email.clone().unwrap_or_else(|| "—".into())}</dd></div>
                    <div class="kv-row"><dt>"Sign-in method"</dt><dd>{format::humanize_kind(&u.auth_source)}</dd></div>
                    <div class="kv-row"><dt>"Member since"</dt><dd>{format::datetime(u.created_at)}</dd></div>
                    <div class="kv-row">
                        <dt>"Permissions"</dt>
                        <dd>
                            <span class="chip-list">
                                {perms.into_iter().map(|p| view! { <span class="chip">{p}</span> }).collect_view()}
                            </span>
                        </dd>
                    </div>
                </dl>
            }
        })
    };

    view! {
        <PageHeader title="Profile" subtitle="Your account and personal API keys." />
        <div class="stack">
            <Card title="Account" subtitle="Ask an administrator to change your name or permissions.">
                {account}
            </Card>
            <QuotaCard />
            <NotificationPrefsCard />
            <ApiKeysPanel />
        </div>
    }
}

/// Request quota usage for the signed-in user.
#[component]
fn QuotaCard() -> impl IntoView {
    let auth = AuthState::expect();
    let quota = LocalResource::new(move || {
        let id = auth.user.with(|u| u.as_ref().map(|u| u.id));
        async move {
            match id {
                Some(id) => Some(api::get::<QuotaDto>(&format!("/users/{id}/quota")).await),
                None => None,
            }
        }
    });
    let meter = |label: &'static str, noun: &'static str, q: QuotaUsageDto| {
        let body = match q.limit {
            None => view! { <p class="muted">{format!("Unlimited {noun}")}</p> }.into_any(),
            Some(limit) => {
                let pct = (q.used.min(limit) * 100).checked_div(limit).unwrap_or(100) as usize;
                let text = quota_line(&q, noun).unwrap_or_default();
                view! {
                    <div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax=limit aria-valuenow=q.used aria-label=label>
                        <div class="progress-bar" class:full=q.restricted style=format!("width: {pct}%")></div>
                    </div>
                    <p class="muted">{format!("{} used · {text}", q.used)}</p>
                }
                .into_any()
            }
        };
        view! {
            <div class="quota-meter">
                <span class="field-label">{label}</span>
                {body}
            </div>
        }
    };
    view! {
        <Card title="Request quota" subtitle="How many requests you can make in the rolling window.">
            {move || match quota.get().flatten() {
                None => view! { <Skeleton lines=2 /> }.into_any(),
                Some(Err(e)) => view! { <p class="muted">{e.message}</p> }.into_any(),
                Some(Ok(q)) if q.exempt => view! { <p class="muted">"You can request without limits."</p> }.into_any(),
                Some(Ok(q)) => view! {
                    <div class="split-grid">
                        {meter("Movies", "movie requests", q.movie)}
                        {meter("Series", "season requests", q.tv)}
                    </div>
                }
                .into_any(),
            }}
        </Card>
    }
}

/// Personal notification events, delivery targets and watchlist sync.
#[component]
fn NotificationPrefsCard() -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let prefs = LocalResource::new(|| api::get::<NotificationPrefsDto>("/profile/notifications"));
    let events = RwSignal::new(Vec::<String>::new());
    let email = RwSignal::new(String::new());
    let discord = RwSignal::new(String::new());
    let telegram = RwSignal::new(String::new());
    let pushover = RwSignal::new(String::new());
    let wl_movies = RwSignal::new(false);
    let wl_series = RwSignal::new(false);
    let saving = RwSignal::new(false);
    Effect::new(move |_| {
        if let Some(Ok(p)) = prefs.get() {
            events.set(p.events);
            email.set(p.email.unwrap_or_default());
            discord.set(p.discord_user_id.unwrap_or_default());
            telegram.set(p.telegram_chat_id.unwrap_or_default());
            pushover.set(p.pushover_user_key.unwrap_or_default());
            wl_movies.set(p.watchlist_sync_movies);
            wl_series.set(p.watchlist_sync_series);
        }
    });
    let opt = |s: RwSignal<String>| {
        let v = s.get_untracked().trim().to_string();
        (!v.is_empty()).then_some(v)
    };
    let save = move |_| {
        let body = NotificationPrefsDto {
            events: events.get_untracked(),
            email: opt(email),
            discord_user_id: opt(discord),
            telegram_chat_id: opt(telegram),
            pushover_user_key: opt(pushover),
            watchlist_sync_movies: wl_movies.get_untracked(),
            watchlist_sync_series: wl_series.get_untracked(),
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, NotificationPrefsDto>("/profile/notifications", &body).await {
                Ok(_) => toasts.success("Notification preferences saved."),
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };
    let is_manager = move || auth.has(Permissions::MANAGE_REQUESTS) || auth.has(Permissions::MANAGE_ISSUES);
    let is_plex = move || {
        auth.user
            .with(|u| u.as_ref().is_some_and(|u| u.auth_source == "plex"))
    };
    let text_field = |id: &'static str, label: &'static str, sig: RwSignal<String>| {
        view! {
            <div class="field">
                <label for=id>{label} <span class="optional">" (optional)"</span></label>
                <input id=id class="input" prop:value=move || sig.get() on:input=move |e| sig.set(event_target_value(&e)) />
            </div>
        }
    };
    view! {
        <Card title="Notifications" subtitle="What Reelix tells you about, and where.">
            {move || match prefs.get() {
                None => view! { <Skeleton lines=4 /> }.into_any(),
                Some(Err(e)) => view! { <p class="muted">{e.message}</p> }.into_any(),
                Some(Ok(_)) => view! {
                    <div class="form">
                        <div class="perm-grid">
                            {USER_NOTIFICATION_EVENTS.iter()
                                .filter(|(id, _)| is_manager() || !matches!(*id, "request_pending" | "issue_reported"))
                                .map(|(id, label)| {
                                    let id = id.to_string();
                                    let id2 = id.clone();
                                    view! {
                                        <label class="checkbox">
                                            <input type="checkbox" prop:checked=move || events.with(|e| e.contains(&id))
                                                on:change=move |e| {
                                                    let on = event_target_checked(&e);
                                                    let id = id2.clone();
                                                    events.update(|v| { v.retain(|x| *x != id); if on { v.push(id); } });
                                                } />
                                            <span>{*label}</span>
                                        </label>
                                    }
                                }).collect_view()}
                        </div>
                        <div class="field-row">
                            {text_field("np-email", "Email", email)}
                            {text_field("np-discord", "Discord user ID", discord)}
                        </div>
                        <div class="field-row">
                            {text_field("np-telegram", "Telegram chat ID", telegram)}
                            {text_field("np-pushover", "Pushover user key", pushover)}
                        </div>
                        <Show when=is_plex>
                            <label class="checkbox"><input type="checkbox" prop:checked=move || wl_movies.get()
                                on:change=move |e| wl_movies.set(event_target_checked(&e)) /><span>"Request movies from my Plex watchlist"</span></label>
                            <label class="checkbox"><input type="checkbox" prop:checked=move || wl_series.get()
                                on:change=move |e| wl_series.set(event_target_checked(&e)) /><span>"Request series from my Plex watchlist"</span></label>
                        </Show>
                        <div class="form-actions">
                            <button type="button" class="btn btn-primary" on:click=save disabled=move || saving.get()>
                                {move || if saving.get() { "Saving…" } else { "Save preferences" }}
                            </button>
                        </div>
                    </div>
                }
                .into_any(),
            }}
        </Card>
    }
}
