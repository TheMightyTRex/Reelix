//! `/books` — the author library: poster grid or table, filters and sort.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::books::{AuthorDto, Paged};
use reelix_api_types::content::ContentSearchKind;

use crate::api;
use crate::components::books::{BookOptions, start_book_search};
use crate::components::library_common::{SortHeader, ViewMode, bytes, on_entity_changed, pref_get, pref_set};
use crate::components::library_forms::{require_library, with_query};
use crate::components::poster::{MediaState, PosterCard};
use crate::components::{EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge};
use crate::state::Toasts;

const SORTS: [(&str, &str); 4] = [
    ("name", "Name"),
    ("books", "Books"),
    ("size", "Size on disk"),
    ("added", "Date added"),
];

/// Poster state of an author from its statistics.
pub fn author_state(a: &AuthorDto) -> MediaState {
    let s = &a.statistics;
    let files = s.ebook_file_count + s.audiobook_file_count;
    if !a.monitored {
        MediaState::Unmonitored
    } else if files == 0 {
        if s.monitored_count == 0 {
            MediaState::Unmonitored
        } else {
            MediaState::Missing
        }
    } else if files >= s.monitored_count {
        MediaState::Downloaded
    } else {
        MediaState::Partial
    }
}

fn sort_authors(list: &mut [AuthorDto], key: &str, desc: bool) {
    list.sort_by(|a, b| {
        let o = match key {
            "books" => a.statistics.book_count.cmp(&b.statistics.book_count),
            "size" => a.statistics.size_on_disk.cmp(&b.statistics.size_on_disk),
            "added" => a.added.cmp(&b.added),
            _ => std::cmp::Ordering::Equal,
        };
        o.then_with(|| a.sort_name.to_lowercase().cmp(&b.sort_name.to_lowercase()))
    });
    if desc {
        list.reverse();
    }
}

#[component]
pub fn AuthorsPage() -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let opts = BookOptions::load();
    let mode = RwSignal::new(match ViewMode::load("authors") {
        ViewMode::Table => ViewMode::Table,
        _ => ViewMode::Posters,
    });
    let q = RwSignal::new(String::new());
    let quick = RwSignal::new(pref_get("authors.filter").unwrap_or_else(|| "all".into()));
    let sort = RwSignal::new(pref_get("authors.sort").unwrap_or_else(|| "name".into()));
    let desc = RwSignal::new(pref_get("authors.desc").as_deref() == Some("1"));

    Effect::new(move |_| {
        pref_set("authors.filter", &quick.get());
        pref_set("authors.sort", &sort.get());
        pref_set("authors.desc", if desc.get() { "1" } else { "0" });
    });

    let authors = LocalResource::new(move || {
        let mut p: Vec<(&str, String)> = vec![
            ("search", q.get().trim().to_string()),
            ("sort", sort.get()),
            ("dir", if desc.get() { "desc" } else { "asc" }.into()),
        ];
        match quick.get().as_str() {
            "monitored" => p.push(("monitored", "true".into())),
            "unmonitored" => p.push(("monitored", "false".into())),
            _ => {}
        }
        let url = with_query("/authors", &p);
        async move { api::get::<Paged<AuthorDto>>(&url).await }
    });
    on_entity_changed(&["author", "book", "book_file", "bookfile"], true, move || {
        authors.refetch()
    });

    let update_all = move |_| {
        spawn_local(async move {
            match api::post_empty::<EnqueuedJob>("/system/tasks/refresh_authors/run").await {
                Ok(_) => toasts.success("Refreshing every author in the background."),
                Err(e) => toasts.error(format!("Could not start the refresh: {e}")),
            }
        });
    };

    let count_label = move || {
        authors
            .get()
            .and_then(Result::ok)
            .map(|p| format!("{} author(s)", p.total))
            .unwrap_or_default()
    };

    let content = move || {
        let mut items = match authors.get() {
            None => return view! { <Skeleton lines=6 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| authors.refetch()) /> }
                    .into_any();
            }
            Some(Ok(p)) => p.items,
        };
        if items.is_empty() {
            let filtered = quick.get_untracked() != "all" || !q.get_untracked().is_empty();
            return if filtered {
                view! {
                    <div class="card">
                        <EmptyState icon="search" title="No authors match" description="Try clearing the search or filters." />
                    </div>
                }
                .into_any()
            } else {
                view! {
                    <div class="card">
                        <EmptyState icon="books" title="Your book library is empty" description="Add authors or single books from Open Library, or import folders you already have.">
                            <A href="/books/add" attr:class="btn btn-primary"><Icon name="plus" />"Add author"</A>
                            <A href="/books/import" attr:class="btn btn-secondary"><Icon name="folder" />"Import library"</A>
                        </EmptyState>
                    </div>
                }
                .into_any()
            };
        }
        sort_authors(&mut items, &sort.get(), desc.get());
        match mode.get() {
            ViewMode::Table => view! {
                <div class="card">
                    <div class="card-body">
                        <div class="table-wrap">
                            <table class="table">
                                <caption class="visually-hidden">"Authors"</caption>
                                <thead>
                                    <tr>
                                        <th scope="col"><span class="visually-hidden">"Status"</span></th>
                                        <SortHeader label="Name" key="name" sort=sort desc=desc />
                                        <SortHeader label="Books" key="books" sort=sort desc=desc />
                                        <th scope="col">"Ebook profile"</th>
                                        <th scope="col">"Audiobook profile"</th>
                                        <th scope="col">"Files"</th>
                                        <SortHeader label="Size" key="size" sort=sort desc=desc />
                                        <SortHeader label="Added" key="added" sort=sort desc=desc />
                                    </tr>
                                </thead>
                                <tbody>
                                    {items.into_iter().map(|a| {
                                        let state = author_state(&a);
                                        let s = a.statistics.clone();
                                        view! {
                                            <tr>
                                                <td><span class=format!("status-dot {}", state.class()) title=state.label()></span></td>
                                                <td class="cell-strong"><A href=format!("/books/authors/{}", a.id)>{a.name.clone()}</A></td>
                                                <td>{format!("{} ({} monitored)", s.book_count, s.monitored_count)}</td>
                                                <td>{move || opts.profile_name(reelix_api_types::content::BookFormat::Ebook, a.quality_profile_id)}</td>
                                                <td>{move || opts.profile_name(reelix_api_types::content::BookFormat::Audiobook, a.audio_quality_profile_id)}</td>
                                                <td>{format!("{} ebook · {} audio", s.ebook_file_count, s.audiobook_file_count)}</td>
                                                <td>{if s.size_on_disk > 0 { bytes(s.size_on_disk) } else { "—".into() }}</td>
                                                <td>{a.added.format("%Y-%m-%d").to_string()}</td>
                                            </tr>
                                        }
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    </div>
                </div>
            }
            .into_any(),
            _ => view! {
                <div class="poster-grid">
                    {items.into_iter().map(|a| {
                        let s = &a.statistics;
                        let files = s.ebook_file_count + s.audiobook_file_count;
                        view! {
                            <PosterCard
                                href=format!("/books/authors/{}", a.id)
                                title=a.name.clone()
                                subtitle=format!("{} book(s)", s.book_count)
                                poster=a.images.poster.clone()
                                state=author_state(&a)
                                monitored=a.monitored
                                progress=(files, s.monitored_count)
                            />
                        }
                    }).collect_view()}
                </div>
            }
            .into_any(),
        }
    };

    let view_btn = move |m: ViewMode, icon: &'static str| {
        view! {
            <button type="button" class="seg-btn" class:active=move || mode.get() == m
                aria-pressed=move || (mode.get() == m).to_string() title=m.label()
                on:click=move |_| { mode.set(m); pref_set("authors.view", m.key()); }>
                <crate::components::library_common::LibIcon name=icon />
                <span class="seg-label">{m.label()}</span>
            </button>
        }
    };

    view! {
        <PageHeader title="Books">
            <button type="button" class="btn btn-secondary" on:click=update_all title="Refresh metadata for every author">
                <Icon name="refresh" />
                <span>"Update all"</span>
            </button>
            <button type="button" class="btn btn-secondary" title="Search for every missing monitored book"
                on:click=move |_| start_book_search(ContentSearchKind::Missing, Vec::new(), None, "all missing books".into())>
                <Icon name="search" />
                <span>"Search missing"</span>
            </button>
            <A href="/books/add" attr:class="btn btn-primary"><Icon name="plus" />"Add author"</A>
        </PageHeader>

        <div class="lib-toolbar">
            <input class="input grow" type="search" placeholder="Filter by name…" aria-label="Filter by name"
                prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e)) />
            <select class="input" aria-label="Show" on:change=move |e| quick.set(event_target_value(&e))>
                {[("all", "All authors"), ("monitored", "Monitored"), ("unmonitored", "Unmonitored")]
                    .into_iter()
                    .map(|(v, l)| view! { <option value=v selected=move || quick.get() == v>{l}</option> })
                    .collect_view()}
            </select>
            <select class="input" aria-label="Sort by" on:change=move |e| sort.set(event_target_value(&e))>
                {SORTS.into_iter().map(|(v, l)| view! { <option value=v selected=move || sort.get() == v>{l}</option> }).collect_view()}
            </select>
            <button type="button" class="icon-btn" title=move || if desc.get() { "Descending" } else { "Ascending" }
                aria-label="Toggle sort direction" on:click=move |_| desc.update(|d| *d = !*d)>
                {move || if desc.get() { "▼" } else { "▲" }}
            </button>
            <div class="segmented" role="group" aria-label="View">
                {view_btn(ViewMode::Posters, "grid")}
                {view_btn(ViewMode::Table, "table")}
            </div>
        </div>
        <p class="lib-count" aria-live="polite">{count_label}</p>
        {content}
        <WantedSummary />
    }
}

/// Small "wanted" counter linking to the missing list (kept unobtrusive below the grid).
#[component]
fn WantedSummary() -> impl IntoView {
    let wanted = LocalResource::new(|| {
        api::get::<Paged<reelix_api_types::books::BookDto>>("/books/wanted?kind=missing&page_size=1")
    });
    view! {
        {move || wanted.get().and_then(Result::ok).filter(|p| p.total > 0).map(|p| view! {
            <p class="lib-count">
                <StatusBadge tone=crate::components::Tone::Danger label=format!("{} missing book format(s)", p.total) dot=true />
            </p>
        })}
    }
}
