//! `/login` — local, Jellyfin / Emby (username + password), Plex (PIN popup) and OIDC sign-in.

use std::time::Duration;

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};
use reelix_api_types::requests::{
    AuthMethodsDto, MediaServerLoginDto, PlexPinDto, PlexPinExchangeDto, PlexPinRequestDto, PlexPinStatusDto,
};
use reelix_api_types::{LoginRequest, UserDto};

use crate::api;
use crate::components::request_status::url_encode;
use crate::components::{Icon, Logo};
use crate::state::{AuthState, Boot, Toasts};

/// Which username/password backend the form posts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Local,
    Jellyfin,
    Emby,
}

impl Mode {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Local => "/auth/login",
            Self::Jellyfin => "/auth/jellyfin",
            Self::Emby => "/auth/emby",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Local => "Reelix",
            Self::Jellyfin => "Jellyfin",
            Self::Emby => "Emby",
        }
    }
}

/// How long to wait for the user to approve the Plex PIN.
const PLEX_TIMEOUT: Duration = Duration::from_secs(180);
const PLEX_POLL: Duration = Duration::from_secs(2);

#[component]
pub fn LoginPage() -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let query = use_query_map();

    let username = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let remember = RwSignal::new(true);
    let busy = RwSignal::new(false);
    let plex_busy = RwSignal::new(false);
    // An OIDC callback that failed redirects back here with `?error=`.
    let error = RwSignal::new(query.with_untracked(|q| q.get("error")));
    let mode = RwSignal::new(Mode::Local);
    let show_local = RwSignal::new(false);

    let methods = LocalResource::new(|| api::get::<AuthMethodsDto>("/auth/methods"));
    let m = move || methods.get().and_then(Result::ok);
    let local_enabled = move || m().is_none_or(|m| m.local) || show_local.get();
    let any_external = move || m().is_some_and(|m| m.jellyfin || m.emby || m.plex || m.oidc);
    // Without local login, start in the first enabled media-server mode.
    Effect::new(move |_| {
        if let Some(m) = m()
            && !m.local
            && mode.get_untracked() == Mode::Local
        {
            if m.jellyfin {
                mode.set(Mode::Jellyfin);
            } else if m.emby {
                mode.set(Mode::Emby);
            }
        }
    });
    let form_visible = move || mode.get() != Mode::Local || local_enabled();

    {
        let navigate = navigate.clone();
        Effect::new(move |_| {
            if auth.boot.get() == Boot::NeedsSetup {
                navigate("/setup", Default::default());
            } else if auth.is_logged_in() {
                navigate("/", Default::default());
            }
        });
    }

    let signed_in = move |user: UserDto| {
        toasts.success(format!("Welcome back, {}.", user.display_name));
        auth.sign_in(user);
    };

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let user = username.get_untracked().trim().to_string();
        let pass = password.get_untracked();
        if user.is_empty() || pass.is_empty() {
            error.set(Some("Enter your username and password.".into()));
            return;
        }
        let md = mode.get_untracked();
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let res = match md {
                Mode::Local => {
                    let req = LoginRequest {
                        username: user,
                        password: pass,
                        remember_me: remember.get_untracked(),
                    };
                    api::post::<_, UserDto>(md.endpoint(), &req).await
                }
                _ => {
                    let req = MediaServerLoginDto {
                        username: user,
                        password: pass,
                        remember_me: remember.get_untracked(),
                    };
                    api::post::<_, UserDto>(md.endpoint(), &req).await
                }
            };
            match res {
                Ok(user) => signed_in(user),
                Err(e) => {
                    let msg = match e.status {
                        401 => format!("That {} username and password don't match.", md.label()),
                        _ => e.message,
                    };
                    error.set(Some(msg));
                    password.set(String::new());
                }
            }
            busy.set(false);
        });
    };

    let plex_login = move |_| {
        if plex_busy.get_untracked() {
            return;
        }
        error.set(None);
        // Open the popup synchronously (inside the click) so browsers don't block it.
        let popup = web_sys::window().and_then(|w| {
            w.open_with_url_and_target("about:blank", "reelix-plex")
                .ok()
                .flatten()
        });
        plex_busy.set(true);
        spawn_local(async move {
            let pin = match api::post::<_, PlexPinDto>("/auth/plex/pin", &PlexPinRequestDto::default()).await
            {
                Ok(p) => p,
                Err(e) => {
                    if let Some(p) = &popup {
                        let _ = p.close();
                    }
                    error.set(Some(format!("Plex sign-in failed: {}", e.message)));
                    plex_busy.set(false);
                    return;
                }
            };
            match &popup {
                Some(p) => {
                    let _ = p.location().set_href(&pin.auth_url);
                }
                None => {
                    error.set(Some("Allow pop-ups for this site to sign in with Plex.".into()));
                    plex_busy.set(false);
                    return;
                }
            }
            let started = chrono::Utc::now();
            let body = PlexPinExchangeDto {
                remember_me: remember.get_untracked(),
            };
            loop {
                gloo_timers::future::sleep(PLEX_POLL).await;
                if !plex_busy.get_untracked() {
                    break;
                }
                let elapsed = (chrono::Utc::now() - started).to_std().unwrap_or_default();
                if elapsed > PLEX_TIMEOUT {
                    error.set(Some("Plex sign-in timed out. Try again.".into()));
                    break;
                }
                match api::post::<_, PlexPinStatusDto>(&format!("/auth/plex/pin/{}", pin.id), &body).await {
                    Ok(PlexPinStatusDto {
                        authenticated: true,
                        user: Some(user),
                    }) => {
                        signed_in(user);
                        break;
                    }
                    Ok(_) => {
                        // Not approved yet; stop if the user closed the popup.
                        if popup.as_ref().is_some_and(|p| p.closed().unwrap_or(false)) {
                            gloo_timers::future::sleep(PLEX_POLL).await;
                            if let Ok(PlexPinStatusDto {
                                authenticated: true,
                                user: Some(user),
                            }) =
                                api::post::<_, PlexPinStatusDto>(&format!("/auth/plex/pin/{}", pin.id), &body)
                                    .await
                            {
                                signed_in(user);
                            }
                            break;
                        }
                    }
                    Err(e) => {
                        error.set(Some(e.message));
                        break;
                    }
                }
            }
            if let Some(p) = &popup {
                let _ = p.close();
            }
            plex_busy.set(false);
        });
    };

    let oidc_login = move |_| {
        // The server prefixes the URL base itself, so the return path is app-relative.
        let target = api::api_url(&format!("/auth/oidc/login?redirect={}", url_encode("/")));
        if let Some(w) = web_sys::window() {
            let _ = w.location().set_href(&target);
        }
    };

    let mode_tabs = move || {
        let m = m()?;
        let mut modes = Vec::new();
        if m.local || show_local.get() {
            modes.push(Mode::Local);
        }
        if m.jellyfin {
            modes.push(Mode::Jellyfin);
        }
        if m.emby {
            modes.push(Mode::Emby);
        }
        (modes.len() > 1).then(|| {
            view! {
                <div class="segmented auth-modes" role="tablist" aria-label="Account type">
                    {modes.into_iter().map(|md| view! {
                        <button type="button" role="tab" class="seg-btn" class:active=move || mode.get() == md
                            aria-selected=move || (mode.get() == md).to_string()
                            on:click=move |_| { mode.set(md); error.set(None); }>
                            {md.label()}
                        </button>
                    }).collect_view()}
                </div>
            }
        })
    };

    let external_buttons = move || {
        let m = m()?;
        (m.plex || m.oidc).then(|| {
            let label = m.oidc_label.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| "Single sign-on".into());
            view! {
                <div class="auth-divider"><span>"or"</span></div>
                <div class="auth-external">
                    {m.plex.then(|| view! {
                        <button type="button" class="btn btn-secondary btn-block btn-lg btn-plex" on:click=plex_login
                            disabled=move || plex_busy.get()>
                            <span class="auth-logo plex" aria-hidden="true">"▶"</span>
                            {move || if plex_busy.get() { "Waiting for Plex…" } else { "Sign in with Plex" }}
                        </button>
                    })}
                    {move || plex_busy.get().then(|| view! {
                        <button type="button" class="btn btn-ghost btn-sm" on:click=move |_| plex_busy.set(false)>"Cancel Plex sign-in"</button>
                    })}
                    {m.oidc.then(|| view! {
                        <button type="button" class="btn btn-secondary btn-block btn-lg" on:click=oidc_login>
                            <Icon name="shield" />
                            {label.clone()}
                        </button>
                    })}
                </div>
            }
        })
    };

    view! {
        <main class="auth-page">
            <div class="auth-card">
                <div class="auth-brand">
                    <Logo />
                    <span class="brand-name">"Reelix"</span>
                </div>
                <h1 class="auth-title">"Sign in"</h1>
                <p class="auth-subtitle">"Your movies, series and requests in one place."</p>

                {move || {
                    error
                        .get()
                        .map(|msg| {
                            view! {
                                <div class="alert alert-danger" role="alert">
                                    <Icon name="error" />
                                    <div class="alert-body">
                                        <span>{msg}</span>
                                    </div>
                                </div>
                            }
                        })
                }}

                {mode_tabs}
                <Show when=form_visible>
                    <form class="form" on:submit=submit novalidate>
                        <div class="field">
                            <label for="login-username">{move || format!("{} username", mode.get().label())}</label>
                            <input
                                id="login-username"
                                class="input"
                                type="text"
                                autocomplete="username"
                                autocapitalize="none"
                                spellcheck="false"
                                autofocus
                                required
                                prop:value=move || username.get()
                                on:input=move |e| username.set(event_target_value(&e))
                            />
                        </div>
                        <div class="field">
                            <label for="login-password">"Password"</label>
                            <input
                                id="login-password"
                                class="input"
                                type="password"
                                autocomplete="current-password"
                                required
                                prop:value=move || password.get()
                                on:input=move |e| password.set(event_target_value(&e))
                            />
                        </div>
                        <label class="checkbox">
                            <input
                                type="checkbox"
                                prop:checked=move || remember.get()
                                on:change=move |e| remember.set(event_target_checked(&e))
                            />
                            <span>"Keep me signed in for 30 days"</span>
                        </label>
                        <button type="submit" class="btn btn-primary btn-block btn-lg" disabled=move || busy.get()>
                            {move || if busy.get() { "Signing in…".to_string() } else if mode.get() == Mode::Local { "Sign in".to_string() } else { format!("Sign in with {}", mode.get().label()) }}
                        </button>
                    </form>
                </Show>
                {external_buttons}
                <Show when=move || !local_enabled() && any_external()>
                    <button type="button" class="btn btn-ghost btn-sm auth-admin-link" on:click=move |_| { show_local.set(true); mode.set(Mode::Local); }>
                        "Administrator? Sign in with a local account"
                    </button>
                </Show>
            </div>
        </main>
    }
}
