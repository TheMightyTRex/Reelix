//! `/settings/metadata` — TMDB / TVDB keys, language and region, plus book and music
//! metadata / tag writing.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::providers::{MetadataSettingsDto, TestResultDto};

use crate::api;
use crate::components::provider_form::TestResultView;
use crate::components::{Card, ErrorBox, PageHeader, Skeleton};
use crate::pages::books::BookMetadataCard;
use crate::pages::music::MusicMetadataCard;
use crate::pages::system::require_admin;
use crate::state::Toasts;

#[component]
pub fn MetadataPage() -> impl IntoView {
    require_admin();
    let settings = LocalResource::new(|| api::get::<MetadataSettingsDto>("/settings/metadata"));
    view! {
        <PageHeader title="Metadata" subtitle="Where titles, artwork, release dates and episode lists come from, and what Reelix writes into imported books and music." />
        {move || match settings.get() {
            None => view! { <Skeleton lines=5 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) /> }.into_any(),
            Some(Ok(s)) => view! { <MetadataForm initial=s /> }.into_any(),
        }}
        <div class="stack">
            <BookMetadataCard />
            <MusicMetadataCard />
        </div>
    }
}

fn text(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

fn opt(s: String) -> Option<String> {
    Some(s.trim().to_owned()).filter(|s| !s.is_empty())
}

#[component]
fn MetadataForm(initial: MetadataSettingsDto) -> impl IntoView {
    let toasts = Toasts::expect();
    let tmdb_key = RwSignal::new(text(&initial.tmdb_api_key));
    let tmdb_token = RwSignal::new(text(&initial.tmdb_read_token));
    let tvdb_key = RwSignal::new(text(&initial.tvdb_api_key));
    let tvdb_pin = RwSignal::new(text(&initial.tvdb_pin));
    let language = RwSignal::new(initial.language.clone());
    let region = RwSignal::new(text(&initial.region));
    let result = RwSignal::new(None::<TestResultDto>);
    let busy = RwSignal::new(false);

    let body = move || MetadataSettingsDto {
        tmdb_api_key: opt(tmdb_key.get_untracked()),
        tmdb_read_token: opt(tmdb_token.get_untracked()),
        tvdb_api_key: opt(tvdb_key.get_untracked()),
        tvdb_pin: opt(tvdb_pin.get_untracked()),
        language: language.get_untracked(),
        region: opt(region.get_untracked()),
    };
    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let b = body();
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, MetadataSettingsDto>("/settings/metadata", &b).await {
                Ok(_) => toasts.success("Metadata settings saved."),
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            busy.set(false);
        });
    };
    let test = move |_| {
        let b = body();
        busy.set(true);
        result.set(None);
        spawn_local(async move {
            match api::post::<_, TestResultDto>("/settings/metadata/test", &b).await {
                Ok(r) => result.set(Some(r)),
                Err(e) => toasts.error(format!("Test failed: {e}")),
            }
            busy.set(false);
        });
    };
    let secret = |id: &'static str, label: &'static str, sig: RwSignal<String>, hint: &'static str| {
        view! {
            <div class="field">
                <label for=id>{label}</label>
                <input id=id class="input" type="password" autocomplete="new-password" placeholder="Not set"
                    prop:value=move || sig.get() on:input=move |e| sig.set(event_target_value(&e)) />
                <p class="field-hint">{hint}</p>
            </div>
        }
    };

    view! {
        <form class="form" on:submit=save>
            <Card title="TMDB" subtitle="Required. This product uses the TMDB API but is not endorsed or certified by TMDB.">
                {secret("md-tmdb-key", "API key (v3)", tmdb_key, "From themoviedb.org → Settings → API.")}
                {secret("md-tmdb-token", "Read access token (v4)", tmdb_token, "Used instead of the API key when set.")}
            </Card>
            <Card title="TheTVDB" subtitle="Optional; improves anime and absolute/scene numbering.">
                {secret("md-tvdb-key", "API key", tvdb_key, "")}
                {secret("md-tvdb-pin", "Subscriber PIN", tvdb_pin, "Only for user-supported keys.")}
            </Card>
            <Card title="Localisation">
                <div class="field-row">
                    <div class="field">
                        <label for="md-lang">"Language"</label>
                        <input id="md-lang" class="input" placeholder="en-US"
                            prop:value=move || language.get() on:input=move |e| language.set(event_target_value(&e)) />
                    </div>
                    <div class="field">
                        <label for="md-region">"Region"</label>
                        <input id="md-region" class="input" maxlength="2" placeholder="US"
                            prop:value=move || region.get() on:input=move |e| region.set(event_target_value(&e)) />
                        <p class="field-hint">"Country for release dates and certifications."</p>
                    </div>
                </div>
            </Card>
            {move || result.get().map(|r| view! { <TestResultView result=r /> })}
            <div class="form-actions">
                <button type="button" class="btn btn-secondary" on:click=test disabled=move || busy.get()>"Test"</button>
                <button type="submit" class="btn btn-primary" disabled=move || busy.get()>"Save"</button>
            </div>
        </form>
    }
}
