//! `/settings/users` — user table, create local users, edit permissions (role presets +
//! individual bits) and quotas, reset passwords, import users from a media server.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::Permissions;
use reelix_api_types::requests::{
    CreateUserDto, ExternalUserDto, ImportServerDto, ImportUsersDto, ImportUsersResultDto, RolesDto,
    UpdateUserDto, UserDetailDto,
};

use crate::api;
use crate::components::request_status::refetch_on_change;
use crate::components::{
    Card, ConfirmDialog, EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton, StatusBadge, Tone,
};
use crate::format;
use crate::state::auth::role_name;
use crate::state::{AuthState, Toasts};

/// Sends users without MANAGE_USERS away.
fn require_manage_users() {
    let auth = AuthState::expect();
    let navigate = leptos_router::hooks::use_navigate();
    Effect::new(move |_| {
        if auth.is_logged_in() && !auth.has(Permissions::MANAGE_USERS) {
            navigate("/discover", Default::default());
        }
    });
}

fn opt_u32(s: &str) -> Option<u32> {
    s.trim().parse().ok()
}

fn opt_str(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn num(v: Option<u32>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

/// Role preset buttons plus one checkbox per permission bit.
#[component]
pub fn PermissionEditor(
    perms: RwSignal<Permissions>,
    #[prop(into)] roles: Signal<Option<RolesDto>>,
) -> impl IntoView {
    move || {
        let Some(r) = roles.get() else {
            return view! { <Skeleton lines=3 /> }.into_any();
        };
        let presets = r.roles.clone();
        let bits = r.permissions.clone();
        view! {
            <div class="perm-editor">
                <div class="segmented" role="group" aria-label="Role presets">
                    {presets.into_iter().map(|role| {
                        let p = role.permissions;
                        view! {
                            <button type="button" class="seg-btn" class:active=move || perms.get() == p
                                aria-pressed=move || (perms.get() == p).to_string()
                                on:click=move |_| perms.set(p)>{role.name}</button>
                        }
                    }).collect_view()}
                </div>
                <div class="perm-grid">
                    {bits.into_iter().map(|b| {
                        let flag = Permissions::from_bits_truncate(b.bit);
                        let is_admin_bit = flag == Permissions::ADMIN;
                        view! {
                            <label class="perm-item" title=b.description.clone()>
                                <input type="checkbox"
                                    prop:checked=move || perms.get().contains(flag)
                                    disabled=move || !is_admin_bit && perms.get().is_admin()
                                    on:change=move |e| {
                                        let on = event_target_checked(&e);
                                        perms.update(|p| p.set(flag, on));
                                    } />
                                <span>
                                    <span class="perm-label">{b.label.clone()}</span>
                                    <span class="perm-desc">{b.description.clone()}</span>
                                </span>
                            </label>
                        }
                    }).collect_view()}
                </div>
            </div>
        }
        .into_any()
    }
}

#[component]
pub fn UsersPage() -> impl IntoView {
    require_manage_users();
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let users = LocalResource::new(|| api::get::<Vec<UserDetailDto>>("/users"));
    let roles = LocalResource::new(|| api::get::<RolesDto>("/users/roles"));
    let roles_sig = Signal::derive(move || roles.get().and_then(Result::ok));
    refetch_on_change(&["user"], move || users.refetch());

    // ---- edit / create ----
    let edit_open = RwSignal::new(false);
    let editing = RwSignal::new(None::<UserDetailDto>);
    let username = RwSignal::new(String::new());
    let display = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let current_password = RwSignal::new(String::new());
    let perms = RwSignal::new(Permissions::ROLE_USER);
    let mq_limit = RwSignal::new(String::new());
    let mq_days = RwSignal::new(String::new());
    let tq_limit = RwSignal::new(String::new());
    let tq_days = RwSignal::new(String::new());
    let bq_limit = RwSignal::new(String::new());
    let bq_days = RwSignal::new(String::new());
    let uq_limit = RwSignal::new(String::new());
    let uq_days = RwSignal::new(String::new());
    let saving = RwSignal::new(false);

    let open_create = move |_| {
        editing.set(None);
        for s in [
            username, display, email, password, mq_limit, mq_days, tq_limit, tq_days, bq_limit, bq_days,
            uq_limit, uq_days,
        ] {
            s.set(String::new());
        }
        perms.set(Permissions::ROLE_USER);
        edit_open.set(true);
    };
    let open_edit = move |u: UserDetailDto| {
        username.set(u.username.clone());
        display.set(u.display_name.clone());
        email.set(u.email.clone().unwrap_or_default());
        password.set(String::new());
        current_password.set(String::new());
        perms.set(u.permissions);
        mq_limit.set(num(u.movie_quota_limit));
        mq_days.set(num(u.movie_quota_days));
        tq_limit.set(num(u.tv_quota_limit));
        tq_days.set(num(u.tv_quota_days));
        bq_limit.set(num(u.book_quota_limit));
        bq_days.set(num(u.book_quota_days));
        uq_limit.set(num(u.music_quota_limit));
        uq_days.set(num(u.music_quota_days));
        editing.set(Some(u));
        edit_open.set(true);
    };

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let pw = password.get_untracked();
        saving.set(true);
        spawn_local(async move {
            let res = match editing.get_untracked() {
                Some(u) => {
                    let body = UpdateUserDto {
                        display_name: opt_str(&display.get_untracked()),
                        email: opt_str(&email.get_untracked()),
                        avatar: u.avatar.clone(),
                        locale: u.locale.clone(),
                        permissions: perms.get_untracked(),
                        movie_quota_limit: opt_u32(&mq_limit.get_untracked()),
                        movie_quota_days: opt_u32(&mq_days.get_untracked()),
                        tv_quota_limit: opt_u32(&tq_limit.get_untracked()),
                        tv_quota_days: opt_u32(&tq_days.get_untracked()),
                        book_quota_limit: opt_u32(&bq_limit.get_untracked()),
                        book_quota_days: opt_u32(&bq_days.get_untracked()),
                        music_quota_limit: opt_u32(&uq_limit.get_untracked()),
                        music_quota_days: opt_u32(&uq_days.get_untracked()),
                        current_password: (!pw.is_empty())
                            .then(|| current_password.get_untracked())
                            .filter(|c| !c.is_empty()),
                        password: (!pw.is_empty()).then_some(pw),
                    };
                    api::put::<_, UserDetailDto>(&format!("/users/{}", u.id), &body).await
                }
                None => {
                    let body = CreateUserDto {
                        username: username.get_untracked().trim().to_string(),
                        password: pw,
                        email: opt_str(&email.get_untracked()),
                        display_name: opt_str(&display.get_untracked()),
                        permissions: Some(perms.get_untracked()),
                        movie_quota_limit: opt_u32(&mq_limit.get_untracked()),
                        movie_quota_days: opt_u32(&mq_days.get_untracked()),
                        tv_quota_limit: opt_u32(&tq_limit.get_untracked()),
                        tv_quota_days: opt_u32(&tq_days.get_untracked()),
                        book_quota_limit: opt_u32(&bq_limit.get_untracked()),
                        book_quota_days: opt_u32(&bq_days.get_untracked()),
                        music_quota_limit: opt_u32(&uq_limit.get_untracked()),
                        music_quota_days: opt_u32(&uq_days.get_untracked()),
                    };
                    api::post::<_, UserDetailDto>("/users", &body).await
                }
            };
            match res {
                Ok(u) => {
                    toasts.success(format!("Saved {}.", u.display_name));
                    edit_open.set(false);
                    users.refetch();
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    // ---- delete ----
    let delete_target = RwSignal::new(None::<UserDetailDto>);
    let delete_open = RwSignal::new(false);
    let confirm_delete = Callback::new(move |()| {
        let Some(u) = delete_target.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/users/{}", u.id)).await {
                Ok(()) => {
                    toasts.success(format!("Deleted {}.", u.display_name));
                    users.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    // ---- import ----
    let import_open = RwSignal::new(false);
    let servers = LocalResource::new(move || {
        let want = import_open.get();
        async move {
            if want {
                Some(api::get::<Vec<ImportServerDto>>("/users/import/servers").await)
            } else {
                None
            }
        }
    });
    let server_id = RwSignal::new(None::<i64>);
    let preview = LocalResource::new(move || {
        let id = server_id.get();
        async move {
            match id {
                Some(id) => Some(api::get::<Vec<ExternalUserDto>>(&format!("/users/import/{id}")).await),
                None => None,
            }
        }
    });
    let picked = RwSignal::new(Vec::<String>::new());
    Effect::new(move |_| {
        if let Some(Some(Ok(list))) = preview.get() {
            picked.set(
                list.into_iter()
                    .filter(|u| u.existing_user_id.is_none())
                    .map(|u| u.external_id)
                    .collect(),
            );
        }
    });
    let importing = RwSignal::new(false);
    let run_import = move |_| {
        let Some(id) = server_id.get_untracked() else {
            return;
        };
        let body = ImportUsersDto {
            external_ids: Some(picked.get_untracked()),
            permissions: None,
        };
        importing.set(true);
        spawn_local(async move {
            match api::post::<_, ImportUsersResultDto>(&format!("/users/import/{id}"), &body).await {
                Ok(r) => {
                    toasts.success(format!(
                        "Imported {} users ({} skipped).",
                        r.imported.len(),
                        r.skipped
                    ));
                    import_open.set(false);
                    users.refetch();
                }
                Err(e) => toasts.error(format!("Import failed: {e}")),
            }
            importing.set(false);
        });
    };

    let table = move || match users.get() {
        None => view! { <Skeleton lines=5 /> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| users.refetch()) /> }.into_any()
        }
        Some(Ok(list)) if list.is_empty() => view! { <EmptyState icon="user" title="No users" /> }.into_any(),
        Some(Ok(list)) => {
            let me = auth.user.with_untracked(|u| u.as_ref().map(|u| u.id));
            view! {
                <div class="table-wrap">
                    <table class="table">
                        <caption class="visually-hidden">"Users"</caption>
                        <thead><tr>
                            <th scope="col">"User"</th><th scope="col">"Sign-in"</th><th scope="col">"Role"</th>
                            <th scope="col">"Requests"</th><th scope="col">"Quotas"</th><th scope="col">"Created"</th>
                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                        </tr></thead>
                        <tbody>
                            {list.into_iter().map(|u| {
                                let initial = u.display_name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                                let quota = match (u.movie_quota_limit, u.tv_quota_limit) {
                                    (None, None) => "Default".to_string(),
                                    (m, t) => format!("{} movies · {} seasons",
                                        m.map(|v| v.to_string()).unwrap_or_else(|| "∞".into()),
                                        t.map(|v| v.to_string()).unwrap_or_else(|| "∞".into())),
                                };
                                let content_quota = [
                                    u.book_quota_limit.map(|v| format!("{v} books")),
                                    u.music_quota_limit.map(|v| format!("{v} albums")),
                                ]
                                .into_iter()
                                .flatten()
                                .collect::<Vec<_>>();
                                let quota = if content_quota.is_empty() {
                                    quota
                                } else {
                                    format!("{quota} · {}", content_quota.join(" · "))
                                };
                                let tone = if u.permissions.is_admin() { Tone::Accent } else { Tone::Neutral };
                                let is_me = Some(u.id) == me;
                                let (ue, ud) = (u.clone(), u.clone());
                                view! {
                                    <tr>
                                        <td>
                                            <div class="request-title-cell">
                                                <span class="avatar" aria-hidden="true">{initial}</span>
                                                <div>
                                                    <div class="cell-strong">{u.display_name.clone()}</div>
                                                    <div class="cell-sub">{format!("@{}", u.username)}{u.email.clone().map(|e| format!(" · {e}"))}</div>
                                                </div>
                                            </div>
                                        </td>
                                        <td>{format::humanize_kind(&u.auth_source)}</td>
                                        <td><StatusBadge tone=tone label=role_name(u.permissions) /></td>
                                        <td>{u.request_count}</td>
                                        <td class="cell-sub">{quota}</td>
                                        <td>{format::relative(u.created_at)}</td>
                                        <td class="cell-actions">
                                            <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| open_edit(ue.clone())>"Edit"</button>
                                            {(!is_me).then(|| view! {
                                                <button type="button" class="icon-btn icon-btn-sm" aria-label="Delete user" title="Delete user"
                                                    on:click=move |_| { delete_target.set(Some(ud.clone())); delete_open.set(true); }>
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
            }
            .into_any()
        }
    };

    let is_create = move || editing.with(Option::is_none);
    let is_local = move || editing.with(|e| e.as_ref().is_none_or(|u| u.auth_source == "local"));
    // Changing your own password needs the current one (unless the account has none yet).
    let needs_current = move || {
        let me = auth.user.with(|u| u.as_ref().map(|u| u.id));
        editing.with(|e| e.as_ref().is_some_and(|u| Some(u.id) == me && u.has_password))
            && password.with(|p| !p.is_empty())
    };

    view! {
        <PageHeader title="Users" subtitle="Who can sign in, what they may do and how much they may request.">
            <button type="button" class="btn btn-secondary" on:click=move |_| { server_id.set(None); import_open.set(true); }>
                <Icon name="download" />"Import users"
            </button>
            <button type="button" class="btn btn-primary" on:click=open_create><Icon name="plus" />"New user"</button>
        </PageHeader>
        <div class="card"><div class="card-body flush">{table}</div></div>

        <Modal open=edit_open title="User">
            <form class="form" on:submit=save>
                <h3 class="form-section">{move || if is_create() { "New local user".to_string() } else { format!("Edit @{}", username.get()) }}</h3>
                <Show when=is_create>
                    <div class="field">
                        <label for="u-name">"Username"</label>
                        <input id="u-name" class="input" required autocomplete="off" prop:value=move || username.get()
                            on:input=move |e| username.set(event_target_value(&e)) />
                    </div>
                </Show>
                <div class="field-row">
                    <div class="field">
                        <label for="u-display">"Display name " <span class="optional">"(optional)"</span></label>
                        <input id="u-display" class="input" prop:value=move || display.get() on:input=move |e| display.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="u-email">"Email " <span class="optional">"(optional)"</span></label>
                        <input id="u-email" class="input" type="email" prop:value=move || email.get() on:input=move |e| email.set(event_target_value(&e)) />
                    </div>
                </div>
                <Show when=is_local>
                    <div class="field">
                        <label for="u-pass">{move || if is_create() { "Password" } else { "Reset password" }}
                            {move || (!is_create()).then(|| view! { <span class="optional">" (leave blank to keep; signs the user out of other sessions)"</span> })}
                        </label>
                        <input id="u-pass" class="input" type="password" autocomplete="new-password" required=is_create
                            prop:value=move || password.get() on:input=move |e| password.set(event_target_value(&e)) />
                    </div>
                </Show>
                <Show when=needs_current>
                    <div class="field">
                        <label for="u-current-pass">"Current password"</label>
                        <input id="u-current-pass" class="input" type="password" autocomplete="current-password" required
                            prop:value=move || current_password.get() on:input=move |e| current_password.set(event_target_value(&e)) />
                    </div>
                </Show>
                <div class="field">
                    <span class="field-label">"Permissions"</span>
                    <PermissionEditor perms=perms roles=roles_sig />
                </div>
                <div class="field">
                    <span class="field-label">"Quotas " <span class="optional">"(blank = global default)"</span></span>
                    <div class="quota-grid">
                        <label>"Movies"<input class="input input-sm" type="number" min="0" placeholder="limit" prop:value=move || mq_limit.get() on:input=move |e| mq_limit.set(event_target_value(&e)) /></label>
                        <label>"per days"<input class="input input-sm" type="number" min="1" placeholder="days" prop:value=move || mq_days.get() on:input=move |e| mq_days.set(event_target_value(&e)) /></label>
                        <label>"Seasons"<input class="input input-sm" type="number" min="0" placeholder="limit" prop:value=move || tq_limit.get() on:input=move |e| tq_limit.set(event_target_value(&e)) /></label>
                        <label>"per days"<input class="input input-sm" type="number" min="1" placeholder="days" prop:value=move || tq_days.get() on:input=move |e| tq_days.set(event_target_value(&e)) /></label>
                        <label>"Books"<input class="input input-sm" type="number" min="0" placeholder="limit" prop:value=move || bq_limit.get() on:input=move |e| bq_limit.set(event_target_value(&e)) /></label>
                        <label>"per days"<input class="input input-sm" type="number" min="1" placeholder="days" prop:value=move || bq_days.get() on:input=move |e| bq_days.set(event_target_value(&e)) /></label>
                        <label>"Albums"<input class="input input-sm" type="number" min="0" placeholder="limit" prop:value=move || uq_limit.get() on:input=move |e| uq_limit.set(event_target_value(&e)) /></label>
                        <label>"per days"<input class="input input-sm" type="number" min="1" placeholder="days" prop:value=move || uq_days.get() on:input=move |e| uq_days.set(event_target_value(&e)) /></label>
                    </div>
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| edit_open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || saving.get()>{move || if saving.get() { "Saving…" } else { "Save" }}</button>
                </div>
            </form>
        </Modal>

        <Modal open=import_open title="Import users from a media server">
            {move || match servers.get() {
                None | Some(None) => view! { <Skeleton lines=2 /> }.into_any(),
                Some(Some(Err(e))) => view! { <ErrorBox error=e /> }.into_any(),
                Some(Some(Ok(list))) if list.is_empty() => view! {
                    <EmptyState icon="server" title="No media servers" description="Add Jellyfin, Emby or Plex under Settings → Media Servers first." compact=true />
                }.into_any(),
                Some(Some(Ok(list))) => view! {
                    <div class="field">
                        <label for="imp-server">"Server"</label>
                        <select id="imp-server" class="input" on:change=move |e| server_id.set(event_target_value(&e).parse().ok())>
                            <option value="">"Choose a server…"</option>
                            {list.into_iter().map(|s| view! { <option value=s.id.to_string()>{format!("{} ({})", s.name, format::humanize_kind(&s.kind))}</option> }).collect_view()}
                        </select>
                    </div>
                }.into_any(),
            }}
            {move || preview.get().flatten().map(|res| match res {
                Err(e) => view! { <ErrorBox error=e /> }.into_any(),
                Ok(list) => view! {
                    <ul class="import-list">
                        {list.into_iter().map(|u| {
                            let ext = u.external_id.clone();
                            let ext2 = ext.clone();
                            let linked = u.existing_user_id.is_some();
                            view! {
                                <li>
                                    <label class="checkbox">
                                        <input type="checkbox" disabled=linked
                                            prop:checked=move || picked.with(|p| p.contains(&ext))
                                            on:change=move |e| {
                                                let on = event_target_checked(&e);
                                                let id = ext2.clone();
                                                picked.update(|p| { p.retain(|x| *x != id); if on { p.push(id); } });
                                            } />
                                        <span>{u.username.clone()}{u.email.clone().map(|e| format!(" · {e}"))}</span>
                                    </label>
                                    {u.is_admin.then(|| view! { <span class="badge tone-accent">"Server admin"</span> })}
                                    {linked.then(|| view! { <span class="badge tone-neutral">"Already imported"</span> })}
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any(),
            })}
            <p class="muted">"Imported users get the default permissions from Settings → Requests and sign in with their media-server account."</p>
            <div class="modal-footer">
                <button type="button" class="btn btn-ghost" on:click=move |_| import_open.set(false)>"Cancel"</button>
                <button type="button" class="btn btn-primary" on:click=run_import
                    disabled=move || importing.get() || picked.with(Vec::is_empty)>
                    {move || format!("Import {} users", picked.with(Vec::len))}
                </button>
            </div>
        </Modal>

        <ConfirmDialog open=delete_open title="Delete user"
            message=Signal::derive(move || delete_target.with(|u| u.as_ref().map(|u| format!("Delete {}? Their requests and issues are removed too.", u.display_name)).unwrap_or_default()))
            confirm_label="Delete" on_confirm=confirm_delete />
        <Card title="Roles" subtitle="Presets fill the permission checkboxes; you can still customise each user afterwards.">
            <ul class="link-list">
                <li>"Administrator — everything, including settings and system."</li>
                <li>"Manager — library, requests, users and issues."</li>
                <li>"Power user — 4K and advanced requests, auto-approval, sees all requests."</li>
                <li>"User — requests movies and series, reports issues."</li>
            </ul>
        </Card>
    }
}
