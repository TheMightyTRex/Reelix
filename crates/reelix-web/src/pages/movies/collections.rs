//! `/movies/collections` — franchise cards with owned / missing counts and monitoring.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::library::{CollectionDto, CollectionEdit, MediaKind, MinimumAvailability};

use crate::api;
use crate::components::library_common::{LibIcon, on_entity_changed};
use crate::components::library_forms::{
    EnumSelect, LibOptions, MIN_AVAILABILITY, ProfileSelect, RootFolderSelect, require_library,
};
use crate::components::poster::{POSTER_SM, Poster, tmdb_image};
use crate::components::{EmptyState, ErrorBox, Modal, PageHeader, Skeleton, Spinner, StatusBadge, Tone};
use crate::state::Toasts;

#[component]
pub fn CollectionsPage() -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let opts = LibOptions::load();
    let collections = LocalResource::new(|| api::get::<Vec<CollectionDto>>("/collections"));
    on_entity_changed(&["collection", "movie"], false, move || collections.refetch());
    let filter = RwSignal::new(String::new());
    let only_missing = RwSignal::new(false);

    let save = move |id: i64, edit: CollectionEdit, msg: &'static str| {
        spawn_local(async move {
            match api::put::<_, serde_json::Value>(&format!("/collections/{id}"), &edit).await {
                Ok(_) => {
                    toasts.success(msg);
                    collections.refetch();
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };

    // Edit dialog
    let edit_open = RwSignal::new(false);
    let edit_id = RwSignal::new(0i64);
    let profile = RwSignal::new(0i64);
    let root = RwSignal::new(0i64);
    let min_avail = RwSignal::new(MinimumAvailability::Released);
    let search_on_add = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let edit = CollectionEdit {
            monitored: None,
            quality_profile_id: Some(profile.get_untracked()).filter(|p| *p != 0),
            root_folder_id: Some(root.get_untracked()).filter(|r| *r != 0),
            minimum_availability: Some(min_avail.get_untracked()),
            search_on_add: Some(search_on_add.get_untracked()),
            tags: None,
        };
        busy.set(true);
        save(edit_id.get_untracked(), edit, "Collection saved.");
        busy.set(false);
        edit_open.set(false);
    };

    let grid = move || {
        let list = match collections.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| collections.refetch()) /> }
                    .into_any();
            }
            Some(Ok(l)) => l,
        };
        if list.is_empty() {
            return view! {
                <div class="card">
                    <EmptyState icon="movies" title="No collections yet"
                        description="Collections appear automatically when you add a movie that belongs to one (e.g. a trilogy)." />
                </div>
            }
            .into_any();
        }
        let f = filter.get().to_lowercase();
        let missing_only = only_missing.get();
        let shown: Vec<CollectionDto> = list
            .into_iter()
            .filter(|c| f.is_empty() || c.title.to_lowercase().contains(&f))
            .filter(|c| !missing_only || c.missing_movies > 0)
            .collect();
        if shown.is_empty() {
            return view! { <div class="card"><EmptyState icon="search" title="No collections match" /></div> }.into_any();
        }
        view! {
            <div class="collection-grid">
                {shown.into_iter().map(|c| {
                    let id = c.id;
                    let total = c.movies.len();
                    let owned = c.movies.iter().filter(|m| m.has_file).count();
                    let in_lib = c.movies.iter().filter(|m| m.movie_id.is_some()).count();
                    let monitored = c.monitored;
                    let edit_c = c.clone();
                    view! {
                        <article class="collection-card">
                            <header class="collection-head">
                                <div>
                                    <h3 class="cf-name">{c.title.clone()}</h3>
                                    <div class="meta-line">
                                        <span>{format!("{in_lib} of {total} in library")}</span>
                                        <span>{format!("{owned} downloaded")}</span>
                                    </div>
                                </div>
                                {if c.missing_movies > 0 {
                                    view! { <StatusBadge tone=Tone::Warning label=format!("{} missing", c.missing_movies) /> }.into_any()
                                } else {
                                    view! { <StatusBadge tone=Tone::Success label="Complete" /> }.into_any()
                                }}
                            </header>
                            <div class="collection-posters">
                                {c.movies.into_iter().map(|m| {
                                    let label = format!("{}{}", m.title, m.year.map(|y| format!(" ({y})")).unwrap_or_default());
                                    let tip = label.clone();
                                    let href = match m.movie_id {
                                        Some(mid) => format!("/movies/{mid}"),
                                        None => format!("/movies/add?term=tmdb:{}", m.tmdb_id),
                                    };
                                    view! {
                                        <A href=href attr:title=tip attr:class=if m.movie_id.is_some() { "" } else { "missing" }>
                                            <Poster title=label.clone() url=tmdb_image(m.poster.as_deref(), POSTER_SM) />
                                        </A>
                                    }
                                }).collect_view()}
                            </div>
                            <div class="cf-actions">
                                <button type="button" class="btn btn-sm" class:btn-primary=monitored class:btn-secondary=!monitored
                                    aria-pressed=monitored.to_string()
                                    title="Monitored collections add their missing movies automatically"
                                    on:click=move |_| save(id, CollectionEdit { monitored: Some(!monitored), ..Default::default() },
                                        if monitored { "Collection unmonitored." } else { "Collection monitored — missing movies will be added." })>
                                    <LibIcon name=if monitored { "bookmark" } else { "bookmark-off" } />
                                    {if monitored { "Monitored" } else { "Unmonitored" }}
                                </button>
                                <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| {
                                    edit_id.set(id);
                                    profile.set(edit_c.quality_profile_id);
                                    root.set(edit_c.root_folder_id);
                                    min_avail.set(edit_c.minimum_availability);
                                    search_on_add.set(edit_c.search_on_add);
                                    edit_open.set(true);
                                }>
                                    <LibIcon name="edit" />
                                    "Edit"
                                </button>
                            </div>
                        </article>
                    }
                }).collect_view()}
            </div>
        }
        .into_any()
    };

    view! {
        <PageHeader title="Collections" subtitle="Franchises in your library. Monitor one to add its missing movies automatically." />
        <div class="lib-toolbar">
            <input class="input grow" type="search" placeholder="Filter collections…" aria-label="Filter collections"
                prop:value=move || filter.get() on:input=move |e| filter.set(event_target_value(&e)) />
            <label class="checkbox">
                <input type="checkbox" prop:checked=move || only_missing.get() on:change=move |e| only_missing.set(event_target_checked(&e)) />
                "Only incomplete"
            </label>
        </div>
        {grid}
        <Modal open=edit_open title="Collection settings">
            <form class="form" on:submit=submit>
                <p class="modal-text">"Used when the collection adds its missing movies."</p>
                <div class="field">
                    <label for="col-root">"Root folder"</label>
                    <RootFolderSelect opts=opts kind=MediaKind::Movie value=root id="col-root" />
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="col-profile">"Quality profile"</label>
                        <ProfileSelect opts=opts value=profile id="col-profile" />
                    </div>
                    <div class="field">
                        <label for="col-avail">"Minimum availability"</label>
                        <EnumSelect table=&MIN_AVAILABILITY value=min_avail id="col-avail" />
                    </div>
                </div>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || search_on_add.get() on:change=move |e| search_on_add.set(event_target_checked(&e)) />
                    "Search for movies when they are added"
                </label>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| edit_open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {move || busy.get().then(|| view! { <Spinner /> })}
                        "Save"
                    </button>
                </div>
            </form>
        </Modal>
    }
}
