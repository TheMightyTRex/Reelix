//! `/movies/{id}` — manage one movie: header, files + media info, cast, actions.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use reelix_api_types::library::{LibrarySearchKind, MediaKind, MovieDto, MovieFileDto};

use super::library::movie_state;
use crate::api;
use crate::components::history_table::HistoryTable;
use crate::components::interactive_search::{InteractiveSearchModal, SearchTarget};
use crate::components::library_bulk::{EditFields, EditMediaModal};
use crate::components::library_common::{LibIcon, bytes, on_entity_changed, runtime};
use crate::components::library_forms::{
    DeleteMediaDialog, LibOptions, MIN_AVAILABILITY, RenameModal, fire, movie_status_label, opt_label,
    require_library, start_search,
};
use crate::components::library_media::{CastRow, MediaFilesTable};
use crate::components::manual_import::{ManualImportModal, ManualImportSource};
use crate::components::poster::{BACKDROP, POSTER_MD, Poster, tmdb_image};
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, Skeleton, StatusBadge};
use crate::state::Toasts;

#[component]
pub fn MovieDetailPage() -> impl IntoView {
    require_library();
    let params = use_params_map();
    let id = move || params.with(|p| p.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0));
    view! { {move || view! { <MovieDetail id=id() /> }} }
}

#[component]
fn MovieDetail(id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let opts = LibOptions::load();
    let movie = LocalResource::new(move || {
        let url = format!("/movies/{id}");
        async move { api::get::<MovieDto>(&url).await }
    });
    let files = LocalResource::new(move || {
        let url = format!("/movies/{id}/files");
        async move { api::get::<Vec<MovieFileDto>>(&url).await }
    });
    on_entity_changed(&["movie", "movie_file", "moviefile"], true, move || {
        movie.refetch();
        files.refetch();
    });

    let tab = RwSignal::new("files");
    let edit_open = RwSignal::new(false);
    let fields = EditFields::new();
    let original_path = RwSignal::new(String::new());
    let delete_open = RwSignal::new(false);
    let rename_open = RwSignal::new(false);
    let file_delete = RwSignal::new(None::<i64>);
    let file_delete_open = RwSignal::new(false);
    let isearch_open = RwSignal::new(false);
    let isearch_target = RwSignal::new(None::<SearchTarget>);
    let mi_open = RwSignal::new(false);
    let mi_source = RwSignal::new(None::<ManualImportSource>);

    let open_edit = move |m: &MovieDto| {
        fields.monitored.set(m.monitored);
        fields.profile.set(m.quality_profile_id);
        fields.min_avail.set(m.minimum_availability);
        fields.path.set(m.path.clone());
        fields.move_files.set(false);
        fields.tags.set(m.tags.clone());
        original_path.set(m.path.clone());
        edit_open.set(true);
    };

    let toggle_monitor = move |m: MovieDto| {
        let body = reelix_api_types::library::MovieEdit {
            monitored: Some(!m.monitored),
            ..Default::default()
        };
        spawn_local(async move {
            match api::put::<_, serde_json::Value>(&format!("/movies/{}", m.id), &body).await {
                Ok(_) => movie.refetch(),
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };

    let delete = Callback::new(move |delete_files: bool| {
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::delete(&format!("/movies/{id}?delete_files={delete_files}")).await {
                Ok(()) => {
                    toasts.success("Movie deleted.");
                    navigate("/movies", Default::default());
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });
    let delete_file = Callback::new(move |()| {
        let Some(fid) = file_delete.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/moviefiles/{fid}")).await {
                Ok(()) => {
                    toasts.success("File deleted.");
                    files.refetch();
                    movie.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete the file: {e}")),
            }
        });
    });

    let header = move || {
        let m = match movie.get() {
            None => return view! { <div class="detail-hero"><Skeleton lines=5 /></div> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| movie.refetch()) /> }
                    .into_any();
            }
            Some(Ok(m)) => m,
        };
        let state = movie_state(&m);
        let backdrop = tmdb_image(m.images.backdrop.as_deref(), BACKDROP)
            .map(|u| format!("background-image: url('{}')", u.replace('\'', "%27")))
            .unwrap_or_default();
        let m_edit = m.clone();
        let m_mon = m.clone();
        let title = m.title.clone();
        let is_title = match m.year {
            Some(y) => format!("{} ({y})", m.title),
            None => m.title.clone(),
        };
        let dates = [
            ("In cinemas", m.in_cinemas),
            ("Digital", m.digital_release),
            ("Physical", m.physical_release),
        ];
        let tags = opts.tag_names(&m.tags);
        view! {
            <div class="detail-hero">
                <div class="detail-backdrop" style=backdrop></div>
                <div class="detail-inner">
                    <Poster title=m.title.clone() url=tmdb_image(m.images.poster.as_deref(), POSTER_MD) />
                    <div class="detail-info">
                        <h1 class="detail-title">
                            {m.title.clone()}
                            {m.year.map(|y| view! { <span class="year">{format!(" ({y})")}</span> })}
                        </h1>
                        {m.tagline.clone().filter(|t| !t.is_empty()).map(|t| view! { <p class="muted">{t}</p> })}
                        <div class="meta-line">
                            <StatusBadge tone=state.tone() label=state.label() dot=true />
                            <span>{movie_status_label(m.status)}</span>
                            {m.runtime_minutes.filter(|r| *r > 0).map(|r| view! { <span>{runtime(r)}</span> })}
                            {m.certification.clone().map(|c| view! { <span class="chip">{c}</span> })}
                            {(m.ratings.vote_average > 0.0).then(|| view! { <span class="rating"><Icon name="star" class="icon-fill" />{format!("{:.1}", m.ratings.vote_average)}</span> })}
                            {m.genres.iter().take(4).map(|g| view! { <span class="chip">{g.clone()}</span> }).collect_view()}
                        </div>
                        {m.overview.clone().map(|o| view! { <p class="detail-overview">{o}</p> })}
                        <div class="detail-facts">
                            <div class="fact"><span class="fact-label">"Path"</span><span class="mono">{m.path.clone()}</span></div>
                            <div class="fact"><span class="fact-label">"Profile"</span><span>{opts.profile_name(m.quality_profile_id)}</span></div>
                            <div class="fact"><span class="fact-label">"Availability"</span><span>{opt_label(&MIN_AVAILABILITY, m.minimum_availability)}</span></div>
                            <div class="fact"><span class="fact-label">"Size"</span><span>{if m.size_on_disk > 0 { bytes(m.size_on_disk) } else { "—".into() }}</span></div>
                            {m.movie_file.as_ref().map(|f| view! {
                                <div class="fact"><span class="fact-label">"Quality"</span><span>{f.file.quality_name.clone()}</span></div>
                            })}
                            {dates.into_iter().filter_map(|(l, d)| d.map(|d| view! {
                                <div class="fact"><span class="fact-label">{l}</span><span>{crate::format::date(d)}</span></div>
                            })).collect_view()}
                            {m.collection.clone().map(|c| view! {
                                <div class="fact"><span class="fact-label">"Collection"</span><A href="/movies/collections">{c.title}</A></div>
                            })}
                            {(!tags.is_empty()).then(|| view! {
                                <div class="fact"><span class="fact-label">"Tags"</span>
                                    <span class="chip-list">{tags.into_iter().map(|t| view! { <span class="chip">{t}</span> }).collect_view()}</span>
                                </div>
                            })}
                        </div>
                        <div class="detail-actions">
                            <button type="button" class="btn btn-primary"
                                on:click=move |_| start_search(LibrarySearchKind::Movie, vec![id], None, None, title.clone())>
                                <Icon name="search" />
                                "Search"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| { isearch_target.set(Some(SearchTarget::movie(id, is_title.clone()))); isearch_open.set(true); }>
                                <Icon name="list" />
                                "Interactive"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| fire(format!("/movies/{id}/refresh"), "Refresh queued.".into())>
                                <Icon name="refresh" />
                                "Refresh"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| fire(format!("/movies/{id}/rescan"), "Disk scan queued.".into())>
                                <Icon name="folder" />
                                "Rescan"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| {
                                    mi_source.set(Some(ManualImportSource { movie_id: Some(id), ..Default::default() }));
                                    mi_open.set(true);
                                }>
                                <Icon name="download" />
                                "Manual import"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| rename_open.set(true)>
                                <LibIcon name="rename" />
                                "Rename"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| toggle_monitor(m_mon.clone())>
                                <LibIcon name=if m.monitored { "bookmark-off" } else { "bookmark" } />
                                {if m.monitored { "Unmonitor" } else { "Monitor" }}
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| open_edit(&m_edit)>
                                <LibIcon name="edit" />
                                "Edit"
                            </button>
                            <button type="button" class="btn btn-ghost btn-danger-text" on:click=move |_| delete_open.set(true)>
                                <Icon name="trash" />
                                "Delete"
                            </button>
                            {m.homepage.clone().map(|h| view! {
                                <a class="btn btn-ghost" href=h target="_blank" rel="noopener noreferrer"><Icon name="external" />"Website"</a>
                            })}
                            {m.imdb_id.clone().map(|i| view! {
                                <a class="btn btn-ghost" href=format!("https://www.imdb.com/title/{i}/") target="_blank" rel="noopener noreferrer"><Icon name="external" />"IMDb"</a>
                            })}
                        </div>
                    </div>
                </div>
            </div>
        }
        .into_any()
    };

    let tab_btn = move |key: &'static str, label: &'static str| {
        view! {
            <button type="button" class="tab" role="tab" class:active=move || tab.get() == key
                aria-selected=move || (tab.get() == key).to_string() on:click=move |_| tab.set(key)>
                {label}
            </button>
        }
    };

    let tab_body = move || {
        match tab.get() {
        "cast" => match movie.get() {
            Some(Ok(m)) if !m.cast.is_empty() => view! { <CastRow cast=m.cast /> }.into_any(),
            Some(Ok(_)) => view! { <EmptyState icon="user" title="No cast information" compact=true /> }.into_any(),
            _ => view! { <Skeleton lines=2 /> }.into_any(),
        },
        "history" => view! { <HistoryTable movie_id=id /> }.into_any(),
        _ => match files.get() {
            None => view! { <Skeleton lines=3 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
            Some(Ok(list)) if list.is_empty() => view! {
                <EmptyState icon="download" title="No file yet"
                    description="Once a release is imported or found by a disk scan, its file and media info show up here." compact=true />
            }
            .into_any(),
            Some(Ok(list)) => view! {
                <MediaFilesTable
                    files=list.into_iter().map(|f| (None::<String>, f.file)).collect()
                    on_delete=Callback::new(move |fid: i64| { file_delete.set(Some(fid)); file_delete_open.set(true); })
                />
            }
            .into_any(),
        },
    }
    };

    view! {
        {header}
        <div class="card">
            <div class="card-body">
                <div class="tabs" role="tablist">
                    {tab_btn("files", "Files")}
                    {tab_btn("cast", "Cast")}
                    {tab_btn("history", "History")}
                </div>
                {tab_body}
            </div>
        </div>
        <EditMediaModal kind=MediaKind::Movie id=id open=edit_open fields=fields original_path=original_path opts=opts
            on_saved=Callback::new(move |()| movie.refetch()) />
        <RenameModal open=rename_open kind=MediaKind::Movie media_id=id />
        <InteractiveSearchModal open=isearch_open target=isearch_target />
        <ManualImportModal open=mi_open source=mi_source />
        <DeleteMediaDialog open=delete_open title="Delete movie?"
            message=Signal::derive(move || movie.get().and_then(Result::ok).map(|m| format!("“{}” will be removed from the library.", m.title)).unwrap_or_default())
            on_confirm=delete />
        <ConfirmDialog open=file_delete_open title="Delete file?"
            message=Signal::derive(|| "The file is removed from disk (or moved to the recycle bin).".to_string())
            confirm_label="Delete file" on_confirm=delete_file />
    }
}
