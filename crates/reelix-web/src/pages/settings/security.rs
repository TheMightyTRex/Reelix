//! `/settings/security` — enabled login methods (local, Jellyfin, Emby, Plex) and OIDC.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::requests::{ImportServerDto, OidcSettingsDto, SecuritySettingsDto};

use crate::api;
use crate::components::{Card, ErrorBox, PageHeader, Skeleton};
use crate::pages::system::require_admin;
use crate::state::Toasts;

#[component]
pub fn SecuritySettingsPage() -> impl IntoView {
    require_admin();
    let settings = LocalResource::new(|| api::get::<SecuritySettingsDto>("/settings/security"));
    view! {
        <PageHeader title="Security" subtitle="How people sign in to Reelix." />
        {move || match settings.get() {
            None => view! { <Skeleton lines=8 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) /> }.into_any(),
            Some(Ok(s)) => view! { <SecurityForm s=s /> }.into_any(),
        }}
    }
}

#[component]
fn Check(
    value: RwSignal<bool>,
    #[prop(into)] label: String,
    #[prop(optional, into)] hint: Option<String>,
) -> impl IntoView {
    view! {
        <label class="checkbox toggle-row">
            <input type="checkbox" prop:checked=move || value.get() on:change=move |e| value.set(event_target_checked(&e)) />
            <span>
                <span class="perm-label">{label}</span>
                {hint.map(|h| view! { <span class="perm-desc">{h}</span> })}
            </span>
        </label>
    }
}

#[component]
fn ServerSelect(
    id: &'static str,
    kind: &'static str,
    value: RwSignal<Option<i64>>,
    #[prop(into)] servers: Signal<Vec<ImportServerDto>>,
) -> impl IntoView {
    view! {
        <div class="field">
            <label for=id>"Server"</label>
            <select id=id class="input" prop:value=move || value.get().map(|v| v.to_string()).unwrap_or_default()
                on:change=move |e| value.set(event_target_value(&e).parse().ok())>
                <option value="">{format!("First {kind} server")}</option>
                {move || servers.get().into_iter().filter(|s| s.kind == kind)
                    .map(|s| view! { <option value=s.id.to_string()>{s.name}</option> }).collect_view()}
            </select>
        </div>
    }
}

#[component]
fn SecurityForm(s: SecuritySettingsDto) -> impl IntoView {
    let toasts = Toasts::expect();
    let servers_res = LocalResource::new(|| api::get::<Vec<ImportServerDto>>("/users/import/servers"));
    let servers = Signal::derive(move || servers_res.get().and_then(Result::ok).unwrap_or_default());

    let local = RwSignal::new(s.local_login);
    let jellyfin = RwSignal::new(s.jellyfin_login);
    let emby = RwSignal::new(s.emby_login);
    let plex = RwSignal::new(s.plex_login);
    let jf_server = RwSignal::new(s.jellyfin_server_id);
    let emby_server = RwSignal::new(s.emby_server_id);
    let auto_create = RwSignal::new(s.media_server_auto_create);
    let o = s.oidc.clone();
    let oidc_enabled = RwSignal::new(o.enabled);
    let issuer = RwSignal::new(o.issuer_url.clone());
    let client_id = RwSignal::new(o.client_id.clone());
    let secret = RwSignal::new(o.client_secret.clone());
    let scopes = RwSignal::new(o.scopes.join(" "));
    let label = RwSignal::new(o.button_label.clone());
    let claim = RwSignal::new(o.username_claim.clone());
    let oidc_auto = RwSignal::new(o.auto_create);
    let redirect = RwSignal::new(o.redirect_url.clone().unwrap_or_default());
    let saving = RwSignal::new(false);

    let default_callback = format!(
        "{}{}",
        web_sys::window()
            .and_then(|w| w.location().origin().ok())
            .unwrap_or_default(),
        api::api_url("/auth/oidc/callback")
    );

    let save = move |_| {
        let body = SecuritySettingsDto {
            local_login: local.get_untracked(),
            jellyfin_login: jellyfin.get_untracked(),
            emby_login: emby.get_untracked(),
            plex_login: plex.get_untracked(),
            jellyfin_server_id: jf_server.get_untracked(),
            emby_server_id: emby_server.get_untracked(),
            media_server_auto_create: auto_create.get_untracked(),
            oidc: OidcSettingsDto {
                enabled: oidc_enabled.get_untracked(),
                issuer_url: issuer.get_untracked().trim().to_string(),
                client_id: client_id.get_untracked().trim().to_string(),
                client_secret: secret.get_untracked(),
                scopes: scopes
                    .get_untracked()
                    .split([' ', ','])
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                button_label: label.get_untracked().trim().to_string(),
                username_claim: claim.get_untracked(),
                auto_create: oidc_auto.get_untracked(),
                redirect_url: Some(redirect.get_untracked().trim().to_string()).filter(|r| !r.is_empty()),
            },
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, SecuritySettingsDto>("/settings/security", &body).await {
                Ok(saved) => {
                    secret.set(saved.oidc.client_secret);
                    toasts.success("Security settings saved.");
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    view! {
        <div class="stack">
            <Card title="Login methods" subtitle="Buttons shown on the sign-in page.">
                <div class="form">
                    <Check value=local label="Local accounts" hint="Username and password stored in Reelix. Administrators can always sign in locally." />
                    <Check value=jellyfin label="Jellyfin" hint="Sign in with a Jellyfin account." />
                    <Show when=move || jellyfin.get()>
                        <ServerSelect id="sec-jf" kind="jellyfin" value=jf_server servers=servers />
                    </Show>
                    <Check value=emby label="Emby" hint="Sign in with an Emby account." />
                    <Show when=move || emby.get()>
                        <ServerSelect id="sec-emby" kind="emby" value=emby_server servers=servers />
                    </Show>
                    <Check value=plex label="Plex" hint="Sign in with plex.tv; the account must be the owner or a friend of a configured Plex server." />
                    <Check value=auto_create label="Create unknown media-server users on first sign-in"
                        hint="They get the default permissions from Settings → Requests. Otherwise import users first." />
                </div>
            </Card>
            <Card title="Single sign-on (OIDC)" subtitle="Authentik, Authelia, Keycloak, Google and other OpenID Connect providers.">
                <div class="form">
                    <Check value=oidc_enabled label="Enable OpenID Connect" />
                    <div class="field">
                        <label for="oidc-issuer">"Issuer URL"</label>
                        <input id="oidc-issuer" class="input" type="url" placeholder="https://auth.example.com/application/o/reelix/"
                            prop:value=move || issuer.get() on:input=move |e| issuer.set(event_target_value(&e)) />
                    </div>
                    <div class="field-row">
                        <div class="field">
                            <label for="oidc-id">"Client ID"</label>
                            <input id="oidc-id" class="input" autocomplete="off" prop:value=move || client_id.get() on:input=move |e| client_id.set(event_target_value(&e)) />
                        </div>
                        <div class="field">
                            <label for="oidc-secret">"Client secret"</label>
                            <input id="oidc-secret" class="input" type="password" autocomplete="new-password"
                                prop:value=move || secret.get() on:input=move |e| secret.set(event_target_value(&e)) />
                        </div>
                    </div>
                    <div class="field-row">
                        <div class="field">
                            <label for="oidc-scopes">"Extra scopes"</label>
                            <input id="oidc-scopes" class="input" placeholder="email profile" prop:value=move || scopes.get() on:input=move |e| scopes.set(event_target_value(&e)) />
                        </div>
                        <div class="field">
                            <label for="oidc-claim">"Username claim"</label>
                            <select id="oidc-claim" class="input" prop:value=move || claim.get() on:change=move |e| claim.set(event_target_value(&e))>
                                {["preferred_username", "email", "sub", "name", "nickname"].into_iter()
                                    .map(|c| view! { <option value=c>{c}</option> }).collect_view()}
                            </select>
                        </div>
                    </div>
                    <div class="field">
                        <label for="oidc-label">"Button label"</label>
                        <input id="oidc-label" class="input" prop:value=move || label.get() on:input=move |e| label.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="oidc-redirect">"Callback URL override " <span class="optional">"(optional)"</span></label>
                        <input id="oidc-redirect" class="input" type="url" placeholder=default_callback.clone()
                            prop:value=move || redirect.get() on:input=move |e| redirect.set(event_target_value(&e)) />
                        <span class="field-hint">{format!("Register this redirect URI with your provider: {default_callback}")}</span>
                    </div>
                    <Check value=oidc_auto label="Create unknown users on first sign-in" />
                </div>
            </Card>
            <div class="form-actions sticky-actions">
                <button type="button" class="btn btn-primary" on:click=save disabled=move || saving.get()>
                    {move || if saving.get() { "Saving…" } else { "Save changes" }}
                </button>
            </div>
        </div>
    }
}
