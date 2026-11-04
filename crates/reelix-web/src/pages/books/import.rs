//! `/books/import` — scan a book root folder for author folders not in the library, match them
//! to Open Library authors and add them.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::books::{
    AuthorLookupDto, AuthorMonitor, BookImportCandidateDto, BookImportItem, BookLibraryImportRequest,
    LibraryImportResult,
};
use reelix_api_types::content::{BookFormat, ContentKind};
use reelix_api_types::library::LibraryImportMatchRequest;

use crate::api;
use crate::components::books::{AUTHOR_MONITOR, BookOptions, FormatProfileSelect, kind_label, opt_id};
use crate::components::library_forms::{EnumSelect, enc, require_library};
use crate::components::poster::Poster;
use crate::components::{EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton, Spinner};
use crate::state::Toasts;

#[derive(Clone)]
struct Row {
    folder: String,
    path: String,
    kind: ContentKind,
    guessed: Option<String>,
    suggestions: RwSignal<Vec<AuthorLookupDto>>,
    /// Chosen Open Library author id (empty = no match).
    chosen: RwSignal<String>,
    selected: RwSignal<bool>,
    lookup_error: RwSignal<Option<String>>,
}

/// Folders sent per `POST /bookimport/match` (the server accepts up to 25).
const MATCH_BATCH: usize = 20;

/// Best default: the first suggestion that is not in the library yet.
fn first_new(suggestions: &[AuthorLookupDto]) -> String {
    suggestions
        .iter()
        .find(|s| s.author_id.is_none())
        .map(|s| s.openlibrary_id.clone())
        .unwrap_or_default()
}

impl Row {
    fn new(c: BookImportCandidateDto) -> Self {
        let first = first_new(&c.suggestions);
        Self {
            folder: c.folder,
            path: c.path,
            kind: c.kind,
            guessed: c.guessed_author,
            selected: RwSignal::new(!first.is_empty()),
            chosen: RwSignal::new(first),
            suggestions: RwSignal::new(c.suggestions),
            lookup_error: RwSignal::new(c.lookup_error),
        }
    }

    /// Fills in suggestions looked up after the scan, keeping an author the user already picked.
    fn apply_match(&self, c: BookImportCandidateDto) {
        let first = first_new(&c.suggestions);
        if self.chosen.get_untracked().is_empty() && !first.is_empty() {
            self.chosen.set(first);
            self.selected.set(true);
        }
        self.suggestions.update(|s| {
            for hit in c.suggestions {
                if !s.iter().any(|x| x.openlibrary_id == hit.openlibrary_id) {
                    s.push(hit);
                }
            }
        });
        self.lookup_error.set(c.lookup_error);
    }
}

#[component]
pub fn ImportAuthorsPage() -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let opts = BookOptions::load();
    let root = RwSignal::new(0i64);
    let scan_root = RwSignal::new(0i64);
    let ebook_profile = RwSignal::new(0i64);
    let audio_profile = RwSignal::new(0i64);
    let monitor = RwSignal::new(AuthorMonitor::Existing);
    let busy = RwSignal::new(false);
    let result = RwSignal::new(None::<LibraryImportResult>);

    // Default the root folder to the first book root.
    Effect::new(move |_| {
        if root.get_untracked() == 0
            && let Some(r) = opts.all_roots().first()
        {
            root.set(r.id);
        }
    });
    let root_format = move || {
        opts.all_roots()
            .into_iter()
            .find(|r| r.id == root.get())
            .and_then(|r| r.kind.book_format())
            .unwrap_or(BookFormat::Ebook)
    };

    // Bumped on every scan, so a stale result (or a match batch from an older scan) is ignored.
    let scan_gen = RwSignal::new(0u32);
    // (done, total) while suggestions are being looked up after a scan.
    let matching = RwSignal::new(None::<(usize, usize)>);
    let scan = LocalResource::new(move || {
        let id = scan_root.get();
        let generation = scan_gen.get();
        async move {
            if id == 0 {
                (generation, None)
            } else {
                // Folders only (fast); suggestions follow in batches below.
                let res = api::get::<Vec<BookImportCandidateDto>>(&format!(
                    "/bookimport/scan?root_folder_id={id}&lookup=false"
                ))
                .await
                .map(|list| list.into_iter().map(Row::new).collect::<Vec<_>>());
                if let Ok(rows) = &res {
                    let rows = rows.clone();
                    spawn_local(async move {
                        let total = rows.len();
                        matching.set((total > 0).then_some((0, total)));
                        for (i, chunk) in rows.chunks(MATCH_BATCH).enumerate() {
                            if scan_gen.get_untracked() != generation {
                                return;
                            }
                            let req = LibraryImportMatchRequest {
                                root_folder_id: id,
                                paths: chunk.iter().map(|r| r.path.clone()).collect(),
                            };
                            match api::post::<_, Vec<BookImportCandidateDto>>("/bookimport/match", &req).await
                            {
                                Ok(found) => {
                                    for c in found {
                                        if let Some(row) = chunk.iter().find(|r| r.path == c.path) {
                                            row.apply_match(c);
                                        }
                                    }
                                }
                                Err(e) => {
                                    let msg = format!("Looking up matches failed: {e}");
                                    for row in chunk {
                                        row.lookup_error.set(Some(msg.clone()));
                                    }
                                }
                            }
                            if scan_gen.get_untracked() == generation {
                                matching.set(Some(((i * MATCH_BATCH + chunk.len()).min(total), total)));
                            }
                        }
                        if scan_gen.get_untracked() == generation {
                            matching.set(None);
                        }
                    });
                }
                (generation, Some(res))
            }
        }
    });
    // The current scan's result; `None` while it (or the first scan) is still loading.
    let current = move || scan.get().filter(|(g, _)| *g == scan_gen.get()).map(|(_, r)| r);
    let rows = move || current().flatten().and_then(Result::ok).unwrap_or_default();
    let selected_count = move || {
        rows()
            .iter()
            .filter(|r| r.selected.get() && !r.chosen.get().is_empty())
            .count()
    };

    // Manual search for one row.
    let search_row = RwSignal::new(None::<Row>);
    let search_open = RwSignal::new(false);
    let search_input = RwSignal::new(String::new());
    let search_term = RwSignal::new(String::new());
    let search_results = LocalResource::new(move || {
        let t = search_term.get();
        async move {
            if t.trim().len() < 2 {
                None
            } else {
                Some(api::get::<Vec<AuthorLookupDto>>(&format!("/authors/lookup?term={}", enc(&t))).await)
            }
        }
    });
    let pick = move |a: AuthorLookupDto| {
        if let Some(row) = search_row.get_untracked() {
            let id = a.openlibrary_id.clone();
            row.suggestions.update(|s| {
                if !s.iter().any(|x| x.openlibrary_id == id) {
                    s.insert(0, a.clone());
                }
            });
            row.chosen.set(id);
            row.selected.set(true);
        }
        search_open.set(false);
    };

    let import = move |_| {
        let ep = opt_id(ebook_profile.get_untracked());
        let ap = opt_id(audio_profile.get_untracked());
        let items: Vec<BookImportItem> = rows()
            .iter()
            .filter(|r| r.selected.get_untracked() && !r.chosen.get_untracked().is_empty())
            .map(|r| {
                let audio = r.kind == ContentKind::Audiobook;
                BookImportItem {
                    path: r.path.clone(),
                    openlibrary_id: r.chosen.get_untracked(),
                    quality_profile_id: if audio { None } else { ep },
                    audio_quality_profile_id: if audio { ap } else { None },
                    monitor: monitor.get_untracked(),
                    tags: Vec::new(),
                }
            })
            .collect();
        if items.is_empty() {
            toasts.error("Select at least one matched folder.");
            return;
        }
        if items
            .iter()
            .any(|i| i.quality_profile_id.is_none() && i.audio_quality_profile_id.is_none())
        {
            toasts.error("Choose a quality profile for the folder's format.");
            return;
        }
        let req = BookLibraryImportRequest {
            root_folder_id: scan_root.get_untracked(),
            items,
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, LibraryImportResult>("/bookimport", &req).await {
                Ok(r) => {
                    toasts.success(format!(
                        "Imported {} author(s). Their files are being scanned in the background.",
                        r.added.len()
                    ));
                    result.set(Some(r));
                    scan_gen.update(|g| *g += 1);
                }
                Err(e) => toasts.error(format!("Import failed: {e}")),
            }
            busy.set(false);
        });
    };

    let table = move || {
        match current() {
        None => view! {
            <p class="muted"><Spinner />" Scanning folders…"</p>
            <Skeleton lines=5 />
        }.into_any(),
        Some(None) => view! {
            <EmptyState icon="folder" title="Pick a book root folder"
                description="Reelix lists the author folders that aren't in the library yet and suggests an Open Library author for each." compact=true />
        }
        .into_any(),
        Some(Some(Err(e))) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| scan_gen.update(|g| *g += 1)) /> }.into_any(),
        Some(Some(Ok(list))) if list.is_empty() => view! {
            <EmptyState icon="check" title="Nothing to import" description="Every folder in this root folder already belongs to an author." compact=true />
        }
        .into_any(),
        Some(Some(Ok(list))) => {
            let all = list.clone();
            let errs = list.clone();
            let lookup_error = move || errs.iter().find_map(|r| r.lookup_error.get());
            view! {
                {move || matching.get().map(|(done, total)| view! {
                    <p class="muted"><Spinner />{format!(" Matching folders with Open Library… {done} of {total}")}</p>
                })}
                {move || lookup_error().map(|e| view! {
                    <div class="alert alert-warning" role="alert">
                        <Icon name="warning" />
                        <div class="alert-body">
                            <strong>"Folders couldn't be matched automatically"</strong>
                            <span>{e}</span>
                            <span>"You can still pick an author for each folder with Search."</span>
                        </div>
                    </div>
                })}
                <div class="table-wrap">
                    <table class="table">
                        <caption class="visually-hidden">"Unmapped folders"</caption>
                        <thead>
                            <tr>
                                <th scope="col" class="col-check">
                                    <input type="checkbox" aria-label="Select all"
                                        on:change=move |e| { let on = event_target_checked(&e); for r in &all { r.selected.set(on && !r.chosen.get_untracked().is_empty()); } } />
                                </th>
                                <th scope="col">"Folder"</th>
                                <th scope="col">"Format"</th>
                                <th scope="col">"Author"</th>
                                <th scope="col"></th>
                            </tr>
                        </thead>
                        <tbody>
                            {list.into_iter().map(|r| {
                                let row = r.clone();
                                let (sel, chosen, sugg) = (r.selected, r.chosen, r.suggestions);
                                view! {
                                    <tr class:is-selected=move || sel.get()>
                                        <td class="col-check">
                                            <input type="checkbox" aria-label=format!("Import {}", r.folder)
                                                prop:checked=move || sel.get() disabled=move || chosen.get().is_empty()
                                                on:change=move |e| sel.set(event_target_checked(&e)) />
                                        </td>
                                        <td>
                                            <span class="cell-strong">{r.folder.clone()}</span>
                                            <span class="cell-sub mono">{r.path.clone()}</span>
                                        </td>
                                        <td>{kind_label(r.kind)}</td>
                                        <td>
                                            <select class="input input-sm" aria-label=format!("Author for {}", r.folder)
                                                on:change=move |e| {
                                                    let id = event_target_value(&e);
                                                    sel.set(!id.is_empty());
                                                    chosen.set(id);
                                                }>
                                                <option value="" selected=move || chosen.get().is_empty()>"— No match —"</option>
                                                {move || sugg.get().into_iter().map(|s| {
                                                    let id = s.openlibrary_id.clone();
                                                    let id2 = id.clone();
                                                    let note = if s.author_id.is_some() { " — already in library" } else { "" };
                                                    let extra = s.top_work.clone().map(|w| format!(" ({w})")).unwrap_or_default();
                                                    view! {
                                                        <option value=id selected=move || chosen.get() == id2 disabled=s.author_id.is_some()>
                                                            {format!("{}{extra}{note}", s.name)}
                                                        </option>
                                                    }
                                                }).collect_view()}
                                            </select>
                                        </td>
                                        <td class="cell-actions">
                                            <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| {
                                                let t = row.guessed.clone().unwrap_or_else(|| row.folder.clone());
                                                search_input.set(t.clone());
                                                search_term.set(t);
                                                search_row.set(Some(row.clone()));
                                                search_open.set(true);
                                            }>
                                                <Icon name="search" />
                                                "Search"
                                            </button>
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            }
            .into_any()
        }
    }
    };

    view! {
        <PageHeader title="Import books" subtitle="Bring author folders that already exist on disk into the library." />
        <div class="card">
            <div class="card-body">
                <form class="form" on:submit=move |e: ev::SubmitEvent| {
                    e.prevent_default();
                    result.set(None);
                    matching.set(None);
                    scan_root.set(root.get_untracked());
                    scan_gen.update(|g| *g += 1);
                }>
                    <div class="field-row">
                        <div class="field">
                            <label for="bi-root">"Root folder"</label>
                            <select id="bi-root" class="input" on:change=move |e| root.set(event_target_value(&e).parse().unwrap_or(0))>
                                {move || opts.all_roots().into_iter().map(|r| {
                                    let rid = r.id;
                                    view! { <option value=rid.to_string() selected=move || root.get() == rid>{format!("{} ({})", r.path, kind_label(r.kind))}</option> }
                                }).collect_view()}
                            </select>
                            {move || opts.all_roots().is_empty().then(|| view! {
                                <p class="field-hint text-danger">"No book root folders yet — add one under Settings → Books."</p>
                            })}
                        </div>
                        <div class="field">
                            <label for="bi-monitor">"Monitor"</label>
                            <EnumSelect table=&AUTHOR_MONITOR value=monitor id="bi-monitor" />
                        </div>
                    </div>
                    <div class="field-row">
                        {move || match root_format() {
                            BookFormat::Ebook => view! {
                                <div class="field">
                                    <label for="bi-eprofile">"Ebook quality profile"</label>
                                    <FormatProfileSelect opts=opts format=BookFormat::Ebook value=ebook_profile id="bi-eprofile" none_label="Choose a profile" />
                                </div>
                            }.into_any(),
                            BookFormat::Audiobook => view! {
                                <div class="field">
                                    <label for="bi-aprofile">"Audiobook quality profile"</label>
                                    <FormatProfileSelect opts=opts format=BookFormat::Audiobook value=audio_profile id="bi-aprofile" none_label="Choose a profile" />
                                </div>
                            }.into_any(),
                        }}
                    </div>
                    <div class="lib-toolbar">
                        <button type="submit" class="btn btn-secondary" disabled=move || root.get() == 0>
                            <Icon name="folder" />
                            "Scan folder"
                        </button>
                    </div>
                </form>
            </div>
        </div>
        {move || result.get().filter(|r| !r.errors.is_empty()).map(|r| view! {
            <div class="alert alert-warning" role="alert">
                <Icon name="warning" />
                <div class="alert-body">
                    <strong>{format!("{} folder(s) could not be imported", r.errors.len())}</strong>
                    {r.errors.into_iter().map(|e| view! { <span>{format!("{} — {}", e.path, e.message)}</span> }).collect_view()}
                </div>
            </div>
        })}
        <div class="card import-card">
            <div class="card-body">{table}</div>
        </div>
        <Show when=move || current().flatten().is_some_and(|r| r.is_ok_and(|l| !l.is_empty()))>
            <div class="bulk-bar">
                <span class="bulk-count">{move || match selected_count() {
                    0 => "Nothing selected: choose an author for a folder (or use Search) to import it".to_owned(),
                    n => format!("{n} ready to import"),
                }}</span>
                <div class="bulk-actions">
                    <button type="button" class="btn btn-primary" disabled=move || busy.get() || selected_count() == 0 on:click=import>
                        {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="download" /> }.into_any() }}
                        "Import selected"
                    </button>
                </div>
            </div>
        </Show>

        <Modal open=search_open title="Find the author">
            <div class="modal-wide">
                <form class="lib-toolbar" on:submit=move |e: ev::SubmitEvent| { e.prevent_default(); search_term.set(search_input.get_untracked()); }>
                    <input class="input grow" type="search" aria-label="Search Open Library" autofocus
                        prop:value=move || search_input.get() on:input=move |e| search_input.set(event_target_value(&e)) />
                    <button type="submit" class="btn btn-secondary"><Icon name="search" />"Search"</button>
                </form>
                {move || match search_results.get() {
                    None => view! { <Skeleton lines=3 /> }.into_any(),
                    Some(None) => ().into_any(),
                    Some(Some(Err(e))) => view! { <ErrorBox error=e /> }.into_any(),
                    Some(Some(Ok(list))) if list.is_empty() => view! { <EmptyState icon="search" title="No matches" compact=true /> }.into_any(),
                    Some(Some(Ok(list))) => view! {
                        <div class="lookup-list">
                            {list.into_iter().map(|a| {
                                let a2 = a.clone();
                                view! {
                                    <article class="lookup-item">
                                        <Poster title=a.name.clone() url=a.image.clone() />
                                        <div class="overview-body">
                                            <span class="overview-title">{a.name.clone()}</span>
                                            {a.top_work.clone().map(|w| view! { <p class="overview-text">{w}</p> })}
                                        </div>
                                        <div>
                                            {if a.author_id.is_some() {
                                                view! { <span class="badge tone-success">"In library"</span> }.into_any()
                                            } else {
                                                view! { <button type="button" class="btn btn-sm btn-primary" on:click=move |_| pick(a2.clone())>"Use"</button> }.into_any()
                                            }}
                                        </div>
                                    </article>
                                }
                            }).collect_view()}
                        </div>
                    }.into_any(),
                }}
            </div>
        </Modal>
    }
}
