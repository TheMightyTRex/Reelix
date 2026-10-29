//! `/books/add` — search Open Library for authors or single books and add them.

use std::time::Duration;

use leptos::ev;
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_query_map;
use reelix_api_types::books::{AuthorLookupDto, BookLookupDto};

use crate::api;
use crate::components::books::{AddAuthorModal, AddBookModal, BookOptions};
use crate::components::library_common::Segmented;
use crate::components::library_forms::{enc, require_library};
use crate::components::poster::Poster;
use crate::components::{EmptyState, ErrorBox, Icon, PageHeader, Skeleton};

#[derive(Clone)]
enum Results {
    Authors(Vec<AuthorLookupDto>),
    Books(Vec<BookLookupDto>),
}

impl Results {
    fn is_empty(&self) -> bool {
        match self {
            Self::Authors(a) => a.is_empty(),
            Self::Books(b) => b.is_empty(),
        }
    }
}

#[component]
pub fn AddAuthorPage() -> impl IntoView {
    require_library();
    let query = use_query_map();
    let initial = query.with_untracked(|q| q.get("term").unwrap_or_default());
    let input = RwSignal::new(initial.clone());
    let term = RwSignal::new(initial);
    let what = RwSignal::new("authors".to_string());
    let opts = BookOptions::load();
    let author_target = RwSignal::new(None::<AuthorLookupDto>);
    let author_open = RwSignal::new(false);
    let book_target = RwSignal::new(None::<BookLookupDto>);
    let book_open = RwSignal::new(false);

    let pending = StoredValue::new(0u32);
    let on_input = move |e: ev::Event| {
        let v = event_target_value(&e);
        input.set(v.clone());
        let n = pending.get_value() + 1;
        pending.set_value(n);
        set_timeout(
            move || {
                if pending.try_get_value() == Some(n) {
                    let _ = term.try_set(v.trim().to_string());
                }
            },
            Duration::from_millis(450),
        );
    };

    let results = LocalResource::new(move || {
        let t = term.get();
        let w = what.get();
        async move {
            if t.trim().len() < 2 {
                return None;
            }
            let q = enc(&t);
            Some(if w == "books" {
                api::get::<Vec<BookLookupDto>>(&format!("/books/lookup?term={q}"))
                    .await
                    .map(Results::Books)
            } else {
                api::get::<Vec<AuthorLookupDto>>(&format!("/authors/lookup?term={q}"))
                    .await
                    .map(Results::Authors)
            })
        }
    });

    let list = move || {
        match results.get() {
        None => view! { <Skeleton lines=4 /> }.into_any(),
        Some(None) => view! {
            <div class="card">
                <EmptyState icon="search" title="Find an author or a book"
                    description="Results come from Open Library. Use isbn:…, asin:… or olid:… to look up an id." />
            </div>
        }
        .into_any(),
        Some(Some(Err(e))) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| results.refetch()) /> }.into_any(),
        Some(Some(Ok(r))) if r.is_empty() => view! {
            <div class="card">
                <EmptyState icon="search" title="No matches" description="Try a different spelling or an ISBN." />
            </div>
        }
        .into_any(),
        Some(Some(Ok(Results::Authors(list)))) => view! {
            <div class="lookup-list">
                {list.into_iter().map(|a| {
                    let action = match a.author_id {
                        Some(id) => view! {
                            <A href=format!("/books/authors/{id}") attr:class="btn btn-sm btn-secondary"><Icon name="check" />"In library"</A>
                        }.into_any(),
                        None => {
                            let a2 = a.clone();
                            view! {
                                <button type="button" class="btn btn-sm btn-primary" on:click=move |_| {
                                    author_target.set(Some(a2.clone()));
                                    author_open.set(true);
                                }>
                                    <Icon name="plus" />
                                    "Add"
                                </button>
                            }.into_any()
                        }
                    };
                    view! {
                        <article class="lookup-item">
                            <Poster title=a.name.clone() url=a.image.clone() />
                            <div class="overview-body">
                                <span class="overview-title">{a.name.clone()}</span>
                                <div class="meta-line">
                                    {a.birth_date.clone().map(|b| view! { <span>{format!("Born {b}")}</span> })}
                                    <span>{format!("{} works", a.work_count)}</span>
                                    <span>{a.openlibrary_id.clone()}</span>
                                </div>
                                {a.top_work.clone().map(|w| view! { <p class="overview-text">{format!("Best known for {w}")}</p> })}
                            </div>
                            <div>{action}</div>
                        </article>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
        Some(Some(Ok(Results::Books(list)))) => view! {
            <div class="lookup-list">
                {list.into_iter().map(|b| {
                    let action = match b.book_id {
                        Some(id) => view! {
                            <A href=format!("/books/book/{id}") attr:class="btn btn-sm btn-secondary"><Icon name="check" />"In library"</A>
                        }.into_any(),
                        None => {
                            let b2 = b.clone();
                            view! {
                                <button type="button" class="btn btn-sm btn-primary" on:click=move |_| {
                                    book_target.set(Some(b2.clone()));
                                    book_open.set(true);
                                }>
                                    <Icon name="plus" />
                                    "Add"
                                </button>
                            }.into_any()
                        }
                    };
                    view! {
                        <article class="lookup-item">
                            <Poster title=b.title.clone() url=b.cover.clone() />
                            <div class="overview-body">
                                <span class="overview-title">{b.title.clone()}{b.year.map(|y| format!(" ({y})"))}</span>
                                <div class="meta-line">
                                    {b.author_name.clone().map(|a| view! { <span>{format!("by {a}")}</span> })}
                                    {b.isbn13.clone().map(|i| view! { <span class="mono">{i}</span> })}
                                    <span>{b.openlibrary_work_id.clone()}</span>
                                </div>
                            </div>
                            <div>{action}</div>
                        </article>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="Add books" subtitle="Search Open Library and add an author (with their books) or a single book." />
        <div class="lib-toolbar">
            <div class="search add-search grow">
                <label for="add-term" class="visually-hidden">"Search"</label>
                <input id="add-term" class="input grow" type="search" placeholder="Author name, book title, or isbn:9780765311788" autofocus
                    prop:value=move || input.get() on:input=on_input
                    on:keydown=move |e: ev::KeyboardEvent| if e.key() == "Enter" { term.set(input.get_untracked().trim().to_string()) } />
            </div>
            <Segmented value=what options=vec![("authors", "Authors"), ("books", "Books")] label="Search for" />
        </div>
        {list}
        <AddAuthorModal open=author_open target=author_target opts=opts />
        <AddBookModal open=book_open target=book_target opts=opts />
    }
}
