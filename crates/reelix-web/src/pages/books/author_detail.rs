//! `/books/authors/{id}` — one author: header, actions and books grouped by series with the
//! ebook / audiobook status of each book.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use reelix_api_types::books::{AuthorDto, BookDto, Paged};
use reelix_api_types::content::{BookFormat, ContentSearchKind};

use super::authors::author_state;
use crate::api;
use crate::components::books::{
    AuthorEditModal, BookOptions, BookRenameModal, format_label, format_status, monitored, position_key,
    set_book_monitored, start_book_search, wants,
};
use crate::components::library_common::{LibIcon, bytes, on_entity_changed};
use crate::components::library_forms::{DeleteMediaDialog, fire, require_library};
use crate::components::poster::Poster;
use crate::components::{EmptyState, ErrorBox, Icon, Skeleton, StatusBadge};
use crate::state::Toasts;

#[component]
pub fn AuthorDetailPage() -> impl IntoView {
    require_library();
    let params = use_params_map();
    let id = move || params.with(|p| p.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0));
    view! { {move || view! { <AuthorDetail id=id() /> }} }
}

/// Books grouped by their first series (ordered by position), standalone books last.
fn group_by_series(mut books: Vec<BookDto>) -> Vec<(Option<String>, Vec<BookDto>)> {
    books.sort_by(|a, b| {
        a.release_date
            .cmp(&b.release_date)
            .then_with(|| a.sort_title.cmp(&b.sort_title))
    });
    let mut groups: Vec<(Option<String>, Vec<BookDto>)> = Vec::new();
    let mut other = Vec::new();
    for b in books {
        match b.series.first().map(|s| s.title.clone()) {
            Some(t) => match groups.iter_mut().find(|(g, _)| g.as_deref() == Some(t.as_str())) {
                Some((_, list)) => list.push(b),
                None => groups.push((Some(t), vec![b])),
            },
            None => other.push(b),
        }
    }
    for (_, list) in &mut groups {
        list.sort_by(|a, b| {
            let pa = position_key(a.series.first().and_then(|s| s.position.as_deref()));
            let pb = position_key(b.series.first().and_then(|s| s.position.as_deref()));
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    if !other.is_empty() {
        groups.push((None, other));
    }
    groups
}

#[component]
fn AuthorDetail(id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let opts = BookOptions::load();
    let author =
        LocalResource::new(move || async move { api::get::<AuthorDto>(&format!("/authors/{id}")).await });
    let books = LocalResource::new(move || async move {
        api::get::<Paged<BookDto>>(&format!("/books?author_id={id}")).await
    });
    on_entity_changed(&["author", "book", "book_file", "bookfile"], true, move || {
        author.refetch();
        books.refetch();
    });

    let edit_open = RwSignal::new(false);
    let rename_open = RwSignal::new(false);
    let delete_open = RwSignal::new(false);
    let author_sig = Signal::derive(move || author.get().and_then(Result::ok));

    let toggle_author = move |a: AuthorDto| {
        let edit = reelix_api_types::books::AuthorEdit {
            monitored: Some(!a.monitored),
            ..Default::default()
        };
        spawn_local(async move {
            match api::put::<_, AuthorDto>(&format!("/authors/{}", a.id), &edit).await {
                Ok(_) => author.refetch(),
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };

    let delete = Callback::new(move |delete_files: bool| {
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::delete(&format!("/authors/{id}?delete_files={delete_files}")).await {
                Ok(()) => {
                    toasts.success("Author deleted.");
                    navigate("/books", Default::default());
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    let header = move || {
        let a = match author.get() {
            None => return view! { <div class="detail-hero"><Skeleton lines=5 /></div> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| author.refetch()) /> }
                    .into_any();
            }
            Some(Ok(a)) => a,
        };
        let state = author_state(&a);
        let s = a.statistics.clone();
        let name = a.name.clone();
        let a_mon = a.clone();
        let tags = opts.tag_names(&a.tags);
        let lifespan = match (&a.birth_date, &a.death_date) {
            (Some(b), Some(d)) => Some(format!("{b} – {d}")),
            (Some(b), None) => Some(format!("Born {b}")),
            _ => None,
        };
        let (ep, ap) = (a.quality_profile_id, a.audio_quality_profile_id);
        let format_fact = move |f: BookFormat, profile: Option<i64>, path: Option<String>| {
            view! {
                <div class="fact">
                    <span class="fact-label">{format_label(f)}</span>
                    <span>
                        {move || opts.profile_name(f, profile)}
                        {path.clone().map(|p| view! { <span class="cell-sub mono">{p}</span> })}
                    </span>
                </div>
            }
        };
        view! {
            <div class="detail-hero">
                <div class="detail-inner">
                    <Poster title=a.name.clone() url=a.images.poster.clone() />
                    <div class="detail-info">
                        <h1 class="detail-title">{a.name.clone()}</h1>
                        {lifespan.map(|l| view! { <p class="muted">{l}</p> })}
                        <div class="meta-line">
                            <StatusBadge tone=state.tone() label=state.label() dot=true />
                            <span>{format!("{} book(s), {} monitored", s.book_count, s.monitored_count)}</span>
                            <span>{format!("{} ebook file(s)", s.ebook_file_count)}</span>
                            <span>{format!("{} audiobook(s)", s.audiobook_file_count)}</span>
                            {(s.size_on_disk > 0).then(|| view! { <span>{bytes(s.size_on_disk)}</span> })}
                        </div>
                        {a.overview.clone().map(|o| view! { <p class="detail-overview">{o}</p> })}
                        <div class="detail-facts">
                            {format_fact(BookFormat::Ebook, ep, a.path.clone())}
                            {format_fact(BookFormat::Audiobook, ap, a.audio_path.clone())}
                            <div class="fact"><span class="fact-label">"New books"</span>
                                <span>{match a.monitor_new_items { reelix_api_types::books::NewBookMonitor::All => "Monitored", reelix_api_types::books::NewBookMonitor::None => "Not monitored" }}</span>
                            </div>
                            <div class="fact"><span class="fact-label">"Open Library"</span>
                                <a href=format!("https://openlibrary.org/authors/{}", a.openlibrary_id) target="_blank" rel="noopener noreferrer">{a.openlibrary_id.clone()}</a>
                            </div>
                            {(!tags.is_empty()).then(|| view! {
                                <div class="fact"><span class="fact-label">"Tags"</span>
                                    <span class="chip-list">{tags.into_iter().map(|t| view! { <span class="chip">{t}</span> }).collect_view()}</span>
                                </div>
                            })}
                        </div>
                        <div class="detail-actions">
                            <button type="button" class="btn btn-primary"
                                on:click=move |_| start_book_search(ContentSearchKind::Parent, vec![id], None, name.clone())>
                                <Icon name="search" />
                                "Search monitored"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| fire(format!("/authors/{id}/refresh"), "Refresh queued.".into())>
                                <Icon name="refresh" />
                                "Refresh"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| fire(format!("/authors/{id}/rescan"), "Disk scan queued.".into())>
                                <Icon name="folder" />
                                "Rescan"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| rename_open.set(true)>
                                <LibIcon name="rename" />
                                "Rename"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| toggle_author(a_mon.clone())>
                                <LibIcon name=if a.monitored { "bookmark-off" } else { "bookmark" } />
                                {if a.monitored { "Unmonitor" } else { "Monitor" }}
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| edit_open.set(true)>
                                <LibIcon name="edit" />
                                "Edit"
                            </button>
                            <button type="button" class="btn btn-ghost btn-danger-text" on:click=move |_| delete_open.set(true)>
                                <Icon name="trash" />
                                "Delete"
                            </button>
                        </div>
                    </div>
                </div>
            </div>
        }
        .into_any()
    };

    let book_list = move || {
        let a = author.get().and_then(Result::ok);
        let list = match books.get() {
            None => return view! { <Skeleton lines=4 /> }.into_any(),
            Some(Err(e)) => return view! { <ErrorBox error=e /> }.into_any(),
            Some(Ok(p)) => p.items,
        };
        let Some(a) = a else {
            return view! { <Skeleton lines=4 /> }.into_any();
        };
        if list.is_empty() {
            return view! {
                <div class="card">
                    <EmptyState icon="books" title="No books yet" description="Refresh the author to load their works from Open Library." />
                </div>
            }
            .into_any();
        }
        let wants_e = wants(&a, BookFormat::Ebook);
        let wants_a = wants(&a, BookFormat::Audiobook);
        group_by_series(list)
            .into_iter()
            .map(|(series, items)| {
                let title = series.clone().unwrap_or_else(|| "Other books".into());
                let n = items.len();
                view! {
                    <section class="card">
                        <header class="card-header">
                            <div>
                                <h2 class="card-title">{title}</h2>
                                <p class="card-subtitle">{format!("{n} book(s)")}</p>
                            </div>
                        </header>
                        <div class="card-body">
                            <div class="table-wrap">
                                <table class="table">
                                    <caption class="visually-hidden">"Books"</caption>
                                    <thead>
                                        <tr>
                                            {series.is_some().then(|| view! { <th scope="col">"#"</th> })}
                                            <th scope="col">"Title"</th>
                                            <th scope="col">"Year"</th>
                                            <th scope="col">"Ebook"</th>
                                            <th scope="col">"Audiobook"</th>
                                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {items.into_iter().map(|b| view! {
                                            <BookRow book=b in_series=series.is_some() wants_ebook=wants_e wants_audio=wants_a
                                                on_change=Callback::new(move |()| books.refetch()) />
                                        }).collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        </div>
                    </section>
                }
            })
            .collect_view()
            .into_any()
    };

    view! {
        {header}
        {book_list}
        <AuthorEditModal open=edit_open author=author_sig opts=opts on_saved=Callback::new(move |()| { author.refetch(); books.refetch(); }) />
        <BookRenameModal open=rename_open author_id=id />
        <DeleteMediaDialog open=delete_open title="Delete author?"
            message=Signal::derive(move || author.get().and_then(Result::ok).map(|a| format!("“{}” and all their books will be removed from the library.", a.name)).unwrap_or_default())
            on_confirm=delete />
    }
}

/// One book row: status per format, monitor toggles and a search button.
#[component]
fn BookRow(
    book: BookDto,
    in_series: bool,
    wants_ebook: bool,
    wants_audio: bool,
    on_change: Callback<()>,
) -> impl IntoView {
    let id = book.id;
    let position = book
        .series
        .first()
        .and_then(|s| s.position.clone())
        .unwrap_or_default();
    let cell = |f: BookFormat, wanted: bool| {
        let (tone, label) = format_status(&book, f, wanted);
        let on = monitored(&book, f);
        let what = format!("{} ({})", book.title, format_label(f).to_lowercase());
        view! {
            <td>
                <div class="meta-line">
                    {wanted.then(|| view! {
                        <button type="button" class="icon-btn"
                            title=if on { "Monitored — click to unmonitor" } else { "Unmonitored — click to monitor" }
                            aria-label=format!("{} {}", if on { "Unmonitor" } else { "Monitor" }, what)
                            on:click=move |_| set_book_monitored(vec![id], Some(f), !on, move || on_change.run(()))>
                            <LibIcon name=if on { "bookmark" } else { "bookmark-off" } />
                        </button>
                    })}
                    <StatusBadge tone=tone label=label />
                    {(wanted && on).then(|| {
                        let what = what.clone();
                        view! {
                            <button type="button" class="icon-btn" title="Search" aria-label=format!("Search {what}")
                                on:click=move |_| start_book_search(ContentSearchKind::Item, vec![id], Some(f), what.clone())>
                                <Icon name="search" />
                            </button>
                        }
                    })}
                </div>
            </td>
        }
    };
    let ebook_cell = cell(BookFormat::Ebook, wants_ebook);
    let audio_cell = cell(BookFormat::Audiobook, wants_audio);
    let title = book.title.clone();
    let subtitle = book.subtitle.clone();
    let year = book.year.map(|y| y.to_string()).unwrap_or_default();
    view! {
        <tr>
            {in_series.then(|| view! { <td>{position}</td> })}
            <td class="cell-strong">
                <A href=format!("/books/book/{id}")>{title}</A>
                {subtitle.map(|s| view! { <span class="cell-sub">{s}</span> })}
            </td>
            <td>{year}</td>
            {ebook_cell}
            {audio_cell}
            <td class="cell-actions">
                <A href=format!("/books/book/{id}") attr:class="btn btn-sm btn-ghost">"Details"</A>
            </td>
        </tr>
    }
}
