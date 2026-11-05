//! `/settings/requests` — defaults for approved requests (profiles / root folders per kind and
//! for 4K), auto-approve, partial requests, default permissions and quotas, discovery region,
//! and the book (per format) / album request defaults.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::requests::{RequestDefaultsDto, RequestOptionsDto, RequestSettingsDto, RolesDto};
use reelix_core::BookFormat;

use crate::api;
use crate::components::{Card, ErrorBox, PageHeader, Skeleton};
use crate::pages::settings::users::PermissionEditor;
use crate::pages::system::require_admin;
use crate::state::Toasts;

fn opt_u32(s: &str) -> Option<u32> {
    s.trim().parse().ok()
}

fn opt_str(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Profile + root folder selects for one kind / quality. `kind`: `movie`, `series`, or
/// `ebook` / `audiobook` / `music` / `radio` (book, album and radio request defaults).
#[component]
pub(crate) fn DefaultsEditor(
    #[prop(into)] label: String,
    kind: &'static str,
    is_4k: bool,
    value: RwSignal<RequestDefaultsDto>,
) -> impl IntoView {
    let options = LocalResource::new(move || async move {
        let url = match kind {
            "ebook" | "audiobook" | "music" | "radio" => format!("/requests/media/options?kind={kind}"),
            _ => format!("/requests/options?kind={kind}&is_4k={is_4k}"),
        };
        api::get::<RequestOptionsDto>(&url).await
    });
    let id = format!("defaults-{kind}-{is_4k}");
    view! {
        <fieldset class="defaults-editor">
            <legend class="field-label">{label}</legend>
            {move || match options.get() {
                None => view! { <Skeleton lines=1 /> }.into_any(),
                Some(Err(e)) => view! { <p class="muted">{format!("Options unavailable: {}", e.message)}</p> }.into_any(),
                Some(Ok(o)) => {
                    let id = id.clone();
                    view! {
                        <div class="field-row">
                            <div class="field">
                                <label for=format!("{id}-p")>"Quality profile"</label>
                                <select id=format!("{id}-p") class="input"
                                    prop:value=move || value.with(|v| v.profile_id.map(|p| p.to_string()).unwrap_or_default())
                                    on:change=move |e| { let v = event_target_value(&e).parse().ok(); value.update(|d| d.profile_id = v); }>
                                    <option value="">"Library default"</option>
                                    {o.profiles.iter().map(|p| view! { <option value=p.id.to_string()>{p.name.clone()}</option> }).collect_view()}
                                </select>
                            </div>
                            <div class="field">
                                <label for=format!("{id}-r")>"Root folder"</label>
                                <select id=format!("{id}-r") class="input"
                                    prop:value=move || value.with(|v| v.root_folder_id.map(|p| p.to_string()).unwrap_or_default())
                                    on:change=move |e| { let v = event_target_value(&e).parse().ok(); value.update(|d| d.root_folder_id = v); }>
                                    <option value="">"First root folder"</option>
                                    {o.root_folders.iter().map(|r| view! { <option value=r.id.to_string()>{r.path.clone()}</option> }).collect_view()}
                                </select>
                            </div>
                        </div>
                    }.into_any()
                }
            }}
        </fieldset>
    }
}

#[component]
fn Toggle(
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
pub fn RequestSettingsPage() -> impl IntoView {
    require_admin();
    let settings = LocalResource::new(|| api::get::<RequestSettingsDto>("/settings/requests"));
    view! {
        <PageHeader title="Requests" subtitle="How requests are approved and added to the library." />
        {move || match settings.get() {
            None => view! { <Skeleton lines=8 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) /> }.into_any(),
            Some(Ok(s)) => view! { <RequestSettingsForm s=s /> }.into_any(),
        }}
    }
}

#[component]
fn RequestSettingsForm(s: RequestSettingsDto) -> impl IntoView {
    let toasts = Toasts::expect();
    let roles = LocalResource::new(|| api::get::<RolesDto>("/users/roles"));
    let roles_sig = Signal::derive(move || roles.get().and_then(Result::ok));

    let movie = RwSignal::new(s.movie.clone());
    let movie_4k = RwSignal::new(s.movie_4k.clone());
    let series = RwSignal::new(s.series.clone());
    let series_4k = RwSignal::new(s.series_4k.clone());
    let enable_4k_movie = RwSignal::new(s.enable_4k_movie);
    let enable_4k_series = RwSignal::new(s.enable_4k_series);
    let min_avail = RwSignal::new(s.minimum_availability.clone());
    let series_type = RwSignal::new(s.series_type.clone());
    let season_folder = RwSignal::new(s.season_folder);
    let auto_movie = RwSignal::new(s.auto_approve_movie);
    let auto_series = RwSignal::new(s.auto_approve_series);
    let search = RwSignal::new(s.search_on_approve);
    let partial = RwSignal::new(s.partial_requests);
    let perms = RwSignal::new(s.default_permissions);
    let mq_limit = RwSignal::new(s.movie_quota_limit.map(|v| v.to_string()).unwrap_or_default());
    let mq_days = RwSignal::new(s.movie_quota_days.to_string());
    let tq_limit = RwSignal::new(s.tv_quota_limit.map(|v| v.to_string()).unwrap_or_default());
    let tq_days = RwSignal::new(s.tv_quota_days.to_string());
    let region = RwSignal::new(s.discover_region.clone().unwrap_or_default());
    let language = RwSignal::new(s.discover_language.clone().unwrap_or_default());
    let hide_available = RwSignal::new(s.hide_available);
    let book_ebook = RwSignal::new(s.book_ebook.clone());
    let book_audiobook = RwSignal::new(s.book_audiobook.clone());
    let book_formats = RwSignal::new(s.book_default_formats.clone());
    let music = RwSignal::new(s.music.clone());
    let music_meta = RwSignal::new(s.music_metadata_profile_id);
    let auto_book = RwSignal::new(s.auto_approve_book);
    let auto_music = RwSignal::new(s.auto_approve_music);
    let bq_limit = RwSignal::new(s.book_quota_limit.map(|v| v.to_string()).unwrap_or_default());
    let bq_days = RwSignal::new(s.book_quota_days.to_string());
    let uq_limit = RwSignal::new(s.music_quota_limit.map(|v| v.to_string()).unwrap_or_default());
    let uq_days = RwSignal::new(s.music_quota_days.to_string());
    let radio = RwSignal::new(s.radio.clone());
    let auto_radio = RwSignal::new(s.auto_approve_radio);
    let rq_limit = RwSignal::new(s.radio_quota_limit.map(|v| v.to_string()).unwrap_or_default());
    let rq_days = RwSignal::new(s.radio_quota_days.to_string());
    let saving = RwSignal::new(false);

    let save = move |_| {
        let body = RequestSettingsDto {
            movie: movie.get_untracked(),
            movie_4k: movie_4k.get_untracked(),
            series: series.get_untracked(),
            series_4k: series_4k.get_untracked(),
            enable_4k_movie: enable_4k_movie.get_untracked(),
            enable_4k_series: enable_4k_series.get_untracked(),
            minimum_availability: min_avail.get_untracked(),
            series_type: series_type.get_untracked(),
            season_folder: season_folder.get_untracked(),
            auto_approve_movie: auto_movie.get_untracked(),
            auto_approve_series: auto_series.get_untracked(),
            search_on_approve: search.get_untracked(),
            partial_requests: partial.get_untracked(),
            default_permissions: perms.get_untracked(),
            movie_quota_limit: opt_u32(&mq_limit.get_untracked()),
            movie_quota_days: opt_u32(&mq_days.get_untracked()).unwrap_or(7).max(1),
            tv_quota_limit: opt_u32(&tq_limit.get_untracked()),
            tv_quota_days: opt_u32(&tq_days.get_untracked()).unwrap_or(7).max(1),
            discover_region: opt_str(&region.get_untracked()).map(|r| r.to_uppercase()),
            discover_language: opt_str(&language.get_untracked()),
            hide_available: hide_available.get_untracked(),
            book_ebook: book_ebook.get_untracked(),
            book_audiobook: book_audiobook.get_untracked(),
            book_default_formats: book_formats.get_untracked(),
            music: music.get_untracked(),
            music_metadata_profile_id: music_meta.get_untracked(),
            auto_approve_book: auto_book.get_untracked(),
            auto_approve_music: auto_music.get_untracked(),
            book_quota_limit: opt_u32(&bq_limit.get_untracked()),
            book_quota_days: opt_u32(&bq_days.get_untracked()).unwrap_or(7).max(1),
            music_quota_limit: opt_u32(&uq_limit.get_untracked()),
            music_quota_days: opt_u32(&uq_days.get_untracked()).unwrap_or(7).max(1),
            radio: radio.get_untracked(),
            auto_approve_radio: auto_radio.get_untracked(),
            radio_quota_limit: opt_u32(&rq_limit.get_untracked()),
            radio_quota_days: opt_u32(&rq_days.get_untracked()).unwrap_or(7).max(1),
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, RequestSettingsDto>("/settings/requests", &body).await {
                Ok(_) => toasts.success("Request settings saved."),
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    view! {
        <div class="stack">
            <Card title="Movies" subtitle="Where approved movie requests go.">
                <div class="form">
                    <DefaultsEditor label="Default" kind="movie" is_4k=false value=movie />
                    <Toggle value=enable_4k_movie label="Enable 4K movie requests" hint="Users with “Request 4K” can ask for a separate 4K copy." />
                    <Show when=move || enable_4k_movie.get()>
                        <DefaultsEditor label="4K" kind="movie" is_4k=true value=movie_4k />
                    </Show>
                    <div class="field">
                        <label for="rs-minavail">"Minimum availability"</label>
                        <select id="rs-minavail" class="input" prop:value=move || min_avail.get() on:change=move |e| min_avail.set(event_target_value(&e))>
                            <option value="announced">"Announced"</option>
                            <option value="in_cinemas">"In cinemas"</option>
                            <option value="released">"Released"</option>
                        </select>
                    </div>
                </div>
            </Card>
            <Card title="Series" subtitle="Where approved series requests go.">
                <div class="form">
                    <DefaultsEditor label="Default" kind="series" is_4k=false value=series />
                    <Toggle value=enable_4k_series label="Enable 4K series requests" />
                    <Show when=move || enable_4k_series.get()>
                        <DefaultsEditor label="4K" kind="series" is_4k=true value=series_4k />
                    </Show>
                    <div class="field">
                        <label for="rs-stype">"Series type"</label>
                        <select id="rs-stype" class="input" prop:value=move || series_type.get() on:change=move |e| series_type.set(event_target_value(&e))>
                            <option value="standard">"Standard"</option>
                            <option value="daily">"Daily"</option>
                            <option value="anime">"Anime"</option>
                        </select>
                    </div>
                    <Toggle value=season_folder label="Use season folders" />
                    <Toggle value=partial label="Allow partial requests" hint="Users can pick individual seasons instead of the whole series." />
                </div>
            </Card>
            <Card title="Books" subtitle="Where approved book requests go, per format. New authors get these profiles.">
                <div class="form">
                    <DefaultsEditor label="Ebook" kind="ebook" is_4k=false value=book_ebook />
                    <DefaultsEditor label="Audiobook" kind="audiobook" is_4k=false value=book_audiobook />
                    <fieldset class="field">
                        <legend class="field-label">"Formats preselected in the request dialog"</legend>
                        {BookFormat::ALL.into_iter().map(|f| view! {
                            <label class="checkbox">
                                <input type="checkbox" prop:checked=move || book_formats.with(|v| v.contains(&f))
                                    on:change=move |e| {
                                        let on = event_target_checked(&e);
                                        book_formats.update(|v| { v.retain(|x| *x != f); if on { v.push(f); v.sort(); } });
                                    } />
                                {match f { BookFormat::Ebook => "Ebook", BookFormat::Audiobook => "Audiobook" }}
                            </label>
                        }).collect_view()}
                        <p class="field-hint">"None checked = both formats."</p>
                    </fieldset>
                </div>
            </Card>
            <Card title="Music" subtitle="Where approved album requests go. New artists get this profile and root folder.">
                <div class="form">
                    <DefaultsEditor label="Albums" kind="music" is_4k=false value=music />
                    <MetadataProfileSelect value=music_meta />
                </div>
            </Card>
            <Card title="Radio" subtitle="How shows from approved radio requests are followed: the follow choice is the radio settings' default; no root folder = the radio library folder.">
                <div class="form">
                    <DefaultsEditor label="Shows" kind="radio" is_4k=false value=radio />
                </div>
            </Card>
            <Card title="Approval">
                <div class="form">
                    <Toggle value=auto_movie label="Auto-approve every movie request" hint="Otherwise only users with the auto-approve permission skip review." />
                    <Toggle value=auto_series label="Auto-approve every series request" />
                    <Toggle value=auto_book label="Auto-approve every book request" />
                    <Toggle value=auto_music label="Auto-approve every album request" />
                    <Toggle value=auto_radio label="Auto-approve every radio request" />
                    <Toggle value=search label="Search as soon as a request is approved" />
                </div>
            </Card>
            <Card title="New users" subtitle="Permissions for new local, imported and external-login users.">
                <PermissionEditor perms=perms roles=roles_sig />
            </Card>
            <Card title="Quotas" subtitle="Global defaults; leave the limit blank for unlimited. Users can be given their own limits.">
                <div class="quota-grid">
                    <label>"Movies"<input class="input input-sm" type="number" min="0" placeholder="unlimited" prop:value=move || mq_limit.get() on:input=move |e| mq_limit.set(event_target_value(&e)) /></label>
                    <label>"per days"<input class="input input-sm" type="number" min="1" prop:value=move || mq_days.get() on:input=move |e| mq_days.set(event_target_value(&e)) /></label>
                    <label>"Seasons"<input class="input input-sm" type="number" min="0" placeholder="unlimited" prop:value=move || tq_limit.get() on:input=move |e| tq_limit.set(event_target_value(&e)) /></label>
                    <label>"per days"<input class="input input-sm" type="number" min="1" prop:value=move || tq_days.get() on:input=move |e| tq_days.set(event_target_value(&e)) /></label>
                    <label>"Books"<input class="input input-sm" type="number" min="0" placeholder="unlimited" prop:value=move || bq_limit.get() on:input=move |e| bq_limit.set(event_target_value(&e)) /></label>
                    <label>"per days"<input class="input input-sm" type="number" min="1" prop:value=move || bq_days.get() on:input=move |e| bq_days.set(event_target_value(&e)) /></label>
                    <label>"Albums"<input class="input input-sm" type="number" min="0" placeholder="unlimited" prop:value=move || uq_limit.get() on:input=move |e| uq_limit.set(event_target_value(&e)) /></label>
                    <label>"per days"<input class="input input-sm" type="number" min="1" prop:value=move || uq_days.get() on:input=move |e| uq_days.set(event_target_value(&e)) /></label>
                    <label>"Radio shows"<input class="input input-sm" type="number" min="0" placeholder="unlimited" prop:value=move || rq_limit.get() on:input=move |e| rq_limit.set(event_target_value(&e)) /></label>
                    <label>"per days"<input class="input input-sm" type="number" min="1" prop:value=move || rq_days.get() on:input=move |e| rq_days.set(event_target_value(&e)) /></label>
                </div>

            </Card>
            <Card title="Discovery">
                <div class="form">
                    <div class="field-row">
                        <div class="field">
                            <label for="rs-region">"Region " <span class="optional">"(ISO 3166, e.g. US, GB, DE)"</span></label>
                            <input id="rs-region" class="input" maxlength="2" prop:value=move || region.get() on:input=move |e| region.set(event_target_value(&e)) />
                        </div>
                        <div class="field">
                            <label for="rs-lang">"Language " <span class="optional">"(e.g. en-US)"</span></label>
                            <input id="rs-lang" class="input" prop:value=move || language.get() on:input=move |e| language.set(event_target_value(&e)) />
                        </div>
                    </div>
                    <Toggle value=hide_available label="Hide available titles from Discover rows" />
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

/// Metadata profile for artists added by album requests (`GET /metadataprofiles`).
#[component]
fn MetadataProfileSelect(value: RwSignal<Option<i64>>) -> impl IntoView {
    let profiles = LocalResource::new(|| {
        api::get::<Vec<reelix_api_types::music::MetadataProfileDto>>("/metadataprofiles")
    });
    view! {
        <div class="field">
            <label for="rs-music-meta">"Metadata profile for new artists"</label>
            <select id="rs-music-meta" class="input"
                prop:value=move || value.get().map(|v| v.to_string()).unwrap_or_default()
                on:change=move |e| value.set(event_target_value(&e).parse().ok())>
                <option value="">"First profile"</option>
                {move || profiles.get().and_then(Result::ok).unwrap_or_default().into_iter().map(|p| {
                    let v = p.id.to_string();
                    let sel = value.get_untracked() == Some(p.id);
                    view! { <option value=v selected=sel>{p.name}</option> }
                }).collect_view()}
            </select>
        </div>
    }
}
