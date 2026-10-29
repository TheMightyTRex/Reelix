//! `/books/book/{id}` — one book: details, the ebook and audiobook formats with their files,
//! and the known editions.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use reelix_api_types::books::{AuthorDto, BookDto, BookEdit, BookFileDto, EditionDto};
use reelix_api_types::content::{BookFormat, ContentSearchKind};

use crate::api;
use crate::components::books::{
    file_of, format_label, format_status, monitored, set_book_monitored, start_book_search, wants,
};
use crate::components::library_common::{LibIcon, bytes, on_entity_changed, runtime};
use crate::components::library_forms::require_library;
use crate::components::poster::Poster;
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, Skeleton, StatusBadge};
use crate::state::Toasts;

#[component]
pub fn BookDetailPage() -> impl IntoView {
    require_library();
    let params = use_params_map();
    let id = move || params.with(|p| p.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0));
    view! { {move || view! { <BookDetail id=id() /> }} }
}

#[component]
fn BookDetail(id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let book = LocalResource::new(move || async move { api::get::<BookDto>(&format!("/books/{id}")).await });
    let author = LocalResource::new(move || {
        let author_id = book.get().and_then(Result::ok).map(|b| b.author_id);
        async move {
            match author_id {
                Some(a) => Some(api::get::<AuthorDto>(&format!("/authors/{a}")).await),
                None => None,
            }
        }
    });
    let editions = LocalResource::new(move || async move {
        api::get::<Vec<EditionDto>>(&format!("/books/{id}/editions")).await
    });
    on_entity_changed(&["book", "book_file", "bookfile"], true, move || book.refetch());

    let file_delete = RwSignal::new(None::<i64>);
    let file_delete_open = RwSignal::new(false);
    let delete_file = Callback::new(move |()| {
        let Some(fid) = file_delete.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/bookfiles/{fid}")).await {
                Ok(()) => {
                    toasts.success("File deleted.");
                    book.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete the file: {e}")),
            }
        });
    });
    let prefer_edition = move |edition_id: i64| {
        let edit = BookEdit {
            preferred_edition_id: Some(edition_id),
            ..Default::default()
        };
        spawn_local(async move {
            match api::put::<_, BookDto>(&format!("/books/{id}"), &edit).await {
                Ok(_) => {
                    toasts.success("Preferred edition saved.");
                    editions.refetch();
                }
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };

    let header = move || {
        let b = match book.get() {
            None => return view! { <div class="detail-hero"><Skeleton lines=5 /></div> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| book.refetch()) /> }
                    .into_any();
            }
            Some(Ok(b)) => b,
        };
        let series = b
            .series
            .iter()
            .map(|s| match &s.position {
                Some(p) => format!("{} #{p}", s.title),
                None => s.title.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let credits = b
            .credits
            .iter()
            .filter(|c| c.role.as_deref().is_some_and(|r| r != "author"))
            .map(|c| format!("{} ({})", c.name, c.role.clone().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(", ");
        view! {
            <div class="detail-hero">
                <div class="detail-inner">
                    <Poster title=b.title.clone() url=b.images.poster.clone() />
                    <div class="detail-info">
                        <h1 class="detail-title">
                            {b.title.clone()}
                            {b.year.map(|y| view! { <span class="year">{format!(" ({y})")}</span> })}
                        </h1>
                        {b.subtitle.clone().map(|s| view! { <p class="muted">{s}</p> })}
                        <p>
                            "by "
                            <A href=format!("/books/authors/{}", b.author_id)>{b.author_name.clone()}</A>
                        </p>
                        <div class="meta-line">
                            {(b.ratings.vote_average > 0.0).then(|| view! { <span class="rating"><Icon name="star" class="icon-fill" />{format!("{:.1}", b.ratings.vote_average)}</span> })}
                            {b.genres.iter().take(5).map(|g| view! { <span class="chip">{g.clone()}</span> }).collect_view()}
                        </div>
                        {b.overview.clone().map(|o| view! { <p class="detail-overview">{o}</p> })}
                        <div class="detail-facts">
                            {(!series.is_empty()).then(|| view! { <div class="fact"><span class="fact-label">"Series"</span><span>{series}</span></div> })}
                            {b.release_date.map(|d| view! { <div class="fact"><span class="fact-label">"Published"</span><span>{d.format("%Y-%m-%d").to_string()}</span></div> })}
                            {b.isbn13.clone().map(|i| view! { <div class="fact"><span class="fact-label">"ISBN"</span><span class="mono">{i}</span></div> })}
                            {b.asin.clone().map(|i| view! { <div class="fact"><span class="fact-label">"ASIN"</span><span class="mono">{i}</span></div> })}
                            {b.page_count.map(|p| view! { <div class="fact"><span class="fact-label">"Pages"</span><span>{p}</span></div> })}
                            {b.language.clone().map(|l| view! { <div class="fact"><span class="fact-label">"Language"</span><span>{l}</span></div> })}
                            {(!credits.is_empty()).then(|| view! { <div class="fact"><span class="fact-label">"Credits"</span><span>{credits}</span></div> })}
                            <div class="fact"><span class="fact-label">"Open Library"</span>
                                <a href=format!("https://openlibrary.org/works/{}", b.openlibrary_work_id) target="_blank" rel="noopener noreferrer">{b.openlibrary_work_id.clone()}</a>
                            </div>
                        </div>
                        <div class="detail-actions">
                            <button type="button" class="btn btn-primary"
                                on:click={
                                    let t = b.title.clone();
                                    move |_| start_book_search(ContentSearchKind::Item, vec![id], None, t.clone())
                                }>
                                <Icon name="search" />
                                "Search"
                            </button>
                            // INTERACTIVE_SEARCH
                        </div>
                    </div>
                </div>
            </div>
        }
        .into_any()
    };

    let formats = move || {
        let (Some(Ok(b)), Some(Some(Ok(a)))) = (book.get(), author.get()) else {
            return view! { <Skeleton lines=3 /> }.into_any();
        };
        view! {
            <div class="split-grid">
                {BookFormat::ALL.into_iter().map(|f| view! {
                    <FormatPanel book=b.clone() format=f wanted=wants(&a, f)
                        on_change=Callback::new(move |()| book.refetch())
                        on_delete=Callback::new(move |fid: i64| { file_delete.set(Some(fid)); file_delete_open.set(true); }) />
                }).collect_view()}
            </div>
        }
        .into_any()
    };

    let edition_list = move || {
        match editions.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => {
            view! { <EmptyState icon="books" title="No editions known" compact=true /> }.into_any()
        }
        Some(Ok(list)) => view! {
            <div class="table-wrap">
                <table class="table">
                    <caption class="visually-hidden">"Editions"</caption>
                    <thead>
                        <tr>
                            <th scope="col">"Title"</th>
                            <th scope="col">"Format"</th>
                            <th scope="col">"Publisher"</th>
                            <th scope="col">"Published"</th>
                            <th scope="col">"Language"</th>
                            <th scope="col">"ISBN / ASIN"</th>
                            <th scope="col"><span class="visually-hidden">"Preferred"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        {list.into_iter().map(|e| {
                            let eid = e.id;
                            let fmt = e.physical_format.clone().unwrap_or_else(|| if e.is_audiobook { "Audiobook".into() } else { "—".into() });
                            view! {
                                <tr>
                                    <td class="cell-strong">{e.title.clone()}{e.page_count.map(|p| view! { <span class="cell-sub">{format!("{p} pages")}</span> })}</td>
                                    <td>{fmt}</td>
                                    <td>{e.publisher.clone().unwrap_or_default()}</td>
                                    <td>{e.publish_date.clone().unwrap_or_default()}</td>
                                    <td>{e.language.clone().unwrap_or_default()}</td>
                                    <td class="mono">{e.isbn13.clone().or(e.isbn10.clone()).or(e.asin.clone()).unwrap_or_default()}</td>
                                    <td class="cell-actions">
                                        {if e.monitored {
                                            view! { <StatusBadge tone=crate::components::Tone::Success label="Preferred" /> }.into_any()
                                        } else {
                                            view! { <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| prefer_edition(eid)>"Prefer"</button> }.into_any()
                                        }}
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
        }
        .into_any(),
    }
    };

    view! {
        {header}
        {formats}
        <section class="card">
            <header class="card-header">
                <div>
                    <h2 class="card-title">"Editions"</h2>
                    <p class="card-subtitle">"The preferred edition supplies the metadata written into imported files."</p>
                </div>
            </header>
            <div class="card-body">{edition_list}</div>
        </section>
        <ConfirmDialog open=file_delete_open title="Delete file?"
            message=Signal::derive(|| "The file (or audiobook folder) is removed from disk or moved to the recycle bin.".to_string())
            confirm_label="Delete file" on_confirm=delete_file />
    }
}

/// Status, monitor toggle, search and file details of one format.
#[component]
fn FormatPanel(
    book: BookDto,
    format: BookFormat,
    wanted: bool,
    on_change: Callback<()>,
    on_delete: Callback<i64>,
) -> impl IntoView {
    let id = book.id;
    let (tone, label) = format_status(&book, format, wanted);
    let on = monitored(&book, format);
    let file: Option<BookFileDto> = file_of(&book, format).cloned();
    let what = format!("{} ({})", book.title, format_label(format).to_lowercase());
    let actions = wanted.then(|| {
        view! {
            <div class="detail-actions">
                <button type="button" class="btn btn-sm btn-secondary"
                    on:click=move |_| set_book_monitored(vec![id], Some(format), !on, move || on_change.run(()))>
                    <LibIcon name=if on { "bookmark-off" } else { "bookmark" } />
                    {if on { "Unmonitor" } else { "Monitor" }}
                </button>
                <button type="button" class="btn btn-sm btn-secondary"
                    on:click=move |_| start_book_search(ContentSearchKind::Item, vec![id], Some(format), what.clone())>
                    <Icon name="search" />
                    "Search"
                </button>
            </div>
        }
    });
    let body = match file {
        None if !wanted => view! {
            <p class="field-hint">{format!("The author has no {} quality profile. Edit the author to want this format.", format_label(format).to_lowercase())}</p>
        }
        .into_any(),
        None => view! { <EmptyState icon="download" title="No file yet" compact=true /> }.into_any(),
        Some(f) => {
            let fid = f.id;
            view! {
                <div class="detail-facts">
                    <div class="fact"><span class="fact-label">"Path"</span><span class="mono">{f.path.clone()}</span></div>
                    <div class="fact"><span class="fact-label">"Quality"</span><span>{f.quality_name.clone()}</span></div>
                    <div class="fact"><span class="fact-label">"Size"</span><span>{bytes(f.size)}</span></div>
                    {f.bitrate_kbps.map(|b| view! { <div class="fact"><span class="fact-label">"Bitrate"</span><span>{format!("{b} kbps")}</span></div> })}
                    {f.duration_seconds.map(|d| view! { <div class="fact"><span class="fact-label">"Duration"</span><span>{runtime(d / 60)}</span></div> })}
                    {f.narrator.clone().map(|n| view! { <div class="fact"><span class="fact-label">"Narrator"</span><span>{n}</span></div> })}
                    {f.release_group.clone().map(|g| view! { <div class="fact"><span class="fact-label">"Release group"</span><span>{g}</span></div> })}
                    {(!f.custom_formats.is_empty()).then(|| view! {
                        <div class="fact"><span class="fact-label">"Custom formats"</span>
                            <span class="chip-list">{f.custom_formats.iter().map(|c| view! { <span class="chip">{c.clone()}</span> }).collect_view()}</span>
                        </div>
                    })}
                    <div class="fact"><span class="fact-label">"Added"</span><span>{f.date_added.format("%Y-%m-%d").to_string()}</span></div>
                </div>
                {(f.parts.len() > 1).then(|| view! {
                    <details class="file-parts">
                        <summary>{format!("{} parts", f.parts.len())}</summary>
                        <ol class="stack">
                            {f.parts.iter().map(|p| view! {
                                <li>
                                    <span class="mono">{p.relative_path.clone()}</span>
                                    <span class="cell-sub">{bytes(p.size)}{p.duration_ms.map(|d| format!(" · {}", runtime((d / 60_000) as u32)))}</span>
                                </li>
                            }).collect_view()}
                        </ol>
                    </details>
                })}
                <div class="detail-actions">
                    <button type="button" class="btn btn-sm btn-ghost btn-danger-text" on:click=move |_| on_delete.run(fid)>
                        <Icon name="trash" />
                        "Delete file"
                    </button>
                </div>
            }
            .into_any()
        }
    };
    view! {
        <section class="card">
            <header class="card-header">
                <div>
                    <h2 class="card-title">{format_label(format)}</h2>
                </div>
                <div class="card-actions"><StatusBadge tone=tone label=label dot=true /></div>
            </header>
            <div class="card-body">
                {actions}
                {body}
            </div>
        </section>
    }
}
