//! "Import library": scan a root folder for unmapped folders, match them to TMDB and bulk add.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::library::{
    ImportCandidateDto, ImportErrorDto, LibraryImportItem, LibraryImportMatchRequest, LibraryImportRequest,
    LibraryImportResult, LookupResult, MediaKind, MinimumAvailability, SeriesMonitor, SeriesType,
};

use super::library_common::bytes;
use super::library_forms::{
    EnumSelect, LibOptions, MIN_AVAILABILITY, ProfileSelect, RootFolderSelect, SERIES_MONITOR, SERIES_TYPES,
    enc, require_library,
};
use super::poster::{POSTER_SM, Poster, tmdb_image};
use super::{EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton, Spinner};
use crate::api;
use crate::state::Toasts;

/// One row of the import table.
#[derive(Clone)]
struct Row {
    folder_name: String,
    path: String,
    parsed: String,
    file_count: u32,
    size: u64,
    suggestions: RwSignal<Vec<LookupResult>>,
    /// Chosen TMDB id (0 = no match).
    chosen: RwSignal<u32>,
    selected: RwSignal<bool>,
    lookup_error: RwSignal<Option<String>>,
}

/// Folders sent per `POST /libraryimport/match` (the server accepts up to 25).
const MATCH_BATCH: usize = 20;
/// Folders added per `POST /libraryimport` request.
const IMPORT_BATCH: usize = 25;

impl Row {
    fn new(c: ImportCandidateDto) -> Self {
        // Suggestions already in the library are poor defaults.
        let first = c
            .suggestions
            .iter()
            .find(|s| !s.in_library)
            .and_then(|s| s.tmdb_id)
            .unwrap_or(0);
        let parsed = match c.parsed_year {
            Some(y) => format!("{} ({y})", c.parsed_title),
            None => c.parsed_title.clone(),
        };
        Self {
            folder_name: c.folder.name,
            path: c.folder.path,
            parsed,
            file_count: c.file_count,
            size: c.size,
            suggestions: RwSignal::new(c.suggestions),
            chosen: RwSignal::new(first),
            selected: RwSignal::new(first != 0),
            lookup_error: RwSignal::new(c.lookup_error),
        }
    }

    /// Fills in suggestions looked up after the scan, keeping a match the user already picked.
    fn apply_match(&self, c: ImportCandidateDto) {
        let first = c
            .suggestions
            .iter()
            .find(|s| !s.in_library)
            .and_then(|s| s.tmdb_id)
            .unwrap_or(0);
        if self.chosen.get_untracked() == 0 && first != 0 {
            self.chosen.set(first);
            self.selected.set(true);
        }
        self.suggestions.update(|s| {
            for hit in c.suggestions {
                if !s.iter().any(|x| x.tmdb_id == hit.tmdb_id) {
                    s.push(hit);
                }
            }
        });
        self.lookup_error.set(c.lookup_error);
    }
}

#[component]
pub fn ImportLibraryPage(kind: MediaKind) -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let opts = LibOptions::load();
    let root = RwSignal::new(0i64);
    let scan_root = RwSignal::new(0i64);
    let profile = RwSignal::new(0i64);
    let min_avail = RwSignal::new(MinimumAvailability::Released);
    let monitor = RwSignal::new(SeriesMonitor::All);
    let series_type = RwSignal::new(SeriesType::Standard);
    let search_on_add = RwSignal::new(false);
    let busy = RwSignal::new(false);
    // (done, total) while selected folders are being imported in batches.
    let importing = RwSignal::new(None::<(usize, usize)>);
    let result = RwSignal::new(None::<LibraryImportResult>);

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
                let res = api::get::<Vec<ImportCandidateDto>>(&format!(
                    "/libraryimport/scan?root_folder_id={id}&lookup=false"
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
                            match api::post::<_, Vec<ImportCandidateDto>>("/libraryimport/match", &req).await
                            {
                                Ok(found) => {
                                    for c in found {
                                        if let Some(row) = chunk.iter().find(|r| r.path == c.folder.path) {
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

    // Override search for one row.
    let search_row = RwSignal::new(None::<Row>);
    let search_open = RwSignal::new(false);
    let search_term = RwSignal::new(String::new());
    let search_results = LocalResource::new(move || {
        let t = search_term.get();
        let path = match kind {
            MediaKind::Movie => "/movies/lookup",
            MediaKind::Series => "/series/lookup",
        };
        async move {
            if t.trim().len() < 2 {
                None
            } else {
                Some(api::get::<Vec<LookupResult>>(&format!("{path}?term={}", enc(&t))).await)
            }
        }
    });
    let search_input = RwSignal::new(String::new());

    let selected_count = move || {
        rows()
            .iter()
            .filter(|r| r.selected.get() && r.chosen.get() != 0)
            .count()
    };

    let import = move |_| {
        let items: Vec<LibraryImportItem> = rows()
            .iter()
            .filter(|r| r.selected.get_untracked() && r.chosen.get_untracked() != 0)
            .map(|r| LibraryImportItem {
                path: r.path.clone(),
                tmdb_id: r.chosen.get_untracked(),
                quality_profile_id: profile.get_untracked(),
                monitored: true,
                minimum_availability: min_avail.get_untracked(),
                monitor: monitor.get_untracked(),
                series_type: series_type.get_untracked(),
                season_folder: true,
                tags: Vec::new(),
            })
            .collect();
        if items.is_empty() {
            toasts.error("Select at least one matched folder.");
            return;
        }
        let (root_folder_id, search) = (scan_root.get_untracked(), search_on_add.get_untracked());
        busy.set(true);
        spawn_local(async move {
            // Small batches keep every request short (a reverse proxy may cut long ones off).
            let total = items.len();
            let mut all = LibraryImportResult::default();
            importing.set(Some((0, total)));
            for chunk in items.chunks(IMPORT_BATCH) {
                let req = LibraryImportRequest {
                    root_folder_id,
                    items: chunk.to_vec(),
                    search_on_add: search,
                };
                match api::post::<_, LibraryImportResult>("/libraryimport", &req).await {
                    Ok(r) => {
                        all.added.extend(r.added);
                        all.errors.extend(r.errors);
                    }
                    Err(e) => all.errors.extend(chunk.iter().map(|i| ImportErrorDto {
                        path: i.path.clone(),
                        message: e.to_string(),
                    })),
                }
                importing.update(|p| *p = p.map(|(done, t)| (done + chunk.len(), t)));
            }
            importing.set(None);
            if all.errors.is_empty() {
                toasts.success(format!(
                    "Imported {} item(s). Their files are being scanned in the background.",
                    all.added.len()
                ));
            } else {
                toasts.error(format!(
                    "Imported {} item(s); {} folder(s) could not be imported.",
                    all.added.len(),
                    all.errors.len()
                ));
            }
            result.set(Some(all));
            scan_gen.update(|g| *g += 1);
            busy.set(false);
        });
    };

    let (title, noun) = match kind {
        MediaKind::Movie => ("Import movies", "movie"),
        MediaKind::Series => ("Import series", "series"),
    };

    let table = move || {
        match current() {
        None => view! {
            <p class="muted"><Spinner />" Scanning folders…"</p>
            <Skeleton lines=5 />
        }.into_any(),
        Some(None) => view! {
            <EmptyState icon="folder" title="Pick a root folder"
                description=format!("Reelix lists the folders that don't belong to any {noun} yet and suggests a match for each.") compact=true />
        }
        .into_any(),
        Some(Some(Err(e))) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| scan_gen.update(|g| *g += 1)) /> }.into_any(),
        Some(Some(Ok(list))) if list.is_empty() => view! {
            <EmptyState icon="check" title="Nothing to import" description="Every folder in this root folder is already in your library." compact=true />
        }
        .into_any(),
        Some(Some(Ok(list))) => {
            let all = list.clone();
            let errs = list.clone();
            let lookup_error = move || errs.iter().find_map(|r| r.lookup_error.get());
            view! {
                {move || matching.get().map(|(done, total)| view! {
                    <p class="muted"><Spinner />{format!(" Matching folders with TMDB… {done} of {total}")}</p>
                })}
                {move || lookup_error().map(|e| view! {
                    <div class="alert alert-warning" role="alert">
                        <Icon name="warning" />
                        <div class="alert-body">
                            <strong>"Folders couldn't be matched automatically"</strong>
                            <span>{e}</span>
                            <span>"You can still pick a match for each folder with Search."</span>
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
                                        on:change=move |e| { let on = event_target_checked(&e); for r in &all { r.selected.set(on && r.chosen.get_untracked() != 0); } } />
                                </th>
                                <th scope="col">"Folder"</th>
                                <th scope="col">"Files"</th>
                                <th scope="col">"Match"</th>
                                <th scope="col"></th>
                            </tr>
                        </thead>
                        <tbody>
                            {list.into_iter().map(|r| {
                                let row = r.clone();
                                let sel = r.selected;
                                let chosen = r.chosen;
                                let sugg = r.suggestions;
                                view! {
                                    <tr class:is-selected=move || sel.get()>
                                        <td class="col-check">
                                            <input type="checkbox" aria-label=format!("Import {}", r.folder_name)
                                                prop:checked=move || sel.get() disabled=move || chosen.get() == 0
                                                on:change=move |e| sel.set(event_target_checked(&e)) />
                                        </td>
                                        <td>
                                            <span class="cell-strong">{r.folder_name.clone()}</span>
                                            <span class="cell-sub">{format!("Parsed as {}", r.parsed)}</span>
                                        </td>
                                        <td>
                                            {r.file_count}
                                            <span class="cell-sub">{bytes(r.size)}</span>
                                        </td>
                                        <td>
                                            <select class="input input-sm" aria-label=format!("Match for {}", r.folder_name)
                                                on:change=move |e| {
                                                    let id = event_target_value(&e).parse().unwrap_or(0);
                                                    chosen.set(id);
                                                    sel.set(id != 0);
                                                }>
                                                <option value="0" selected=move || chosen.get() == 0>"— No match —"</option>
                                                {move || sugg.get().into_iter().filter_map(|s| {
                                                    let id = s.tmdb_id?;
                                                    let year = s.year.map(|y| format!(" ({y})")).unwrap_or_default();
                                                    let note = if s.in_library { " — already in library" } else { "" };
                                                    Some(view! {
                                                        <option value=id.to_string() selected=move || chosen.get() == id disabled=s.in_library>
                                                            {format!("{}{year}{note}", s.title)}
                                                        </option>
                                                    })
                                                }).collect_view()}
                                            </select>
                                        </td>
                                        <td class="cell-actions">
                                            <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| {
                                                let t = row.parsed.clone();
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

    let pick = move |r: LookupResult| {
        if let (Some(row), Some(id)) = (search_row.get_untracked(), r.tmdb_id) {
            row.suggestions.update(|s| {
                if !s.iter().any(|x| x.tmdb_id == Some(id)) {
                    s.insert(0, r.clone());
                }
            });
            row.chosen.set(id);
            row.selected.set(true);
        }
        search_open.set(false);
    };

    view! {
        <PageHeader title=title subtitle="Bring folders that already exist on disk into the library." />
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
                            <label for="imp-root">"Root folder"</label>
                            <RootFolderSelect opts=opts kind=kind value=root id="imp-root" />
                        </div>
                        <div class="field">
                            <label for="imp-profile">"Quality profile"</label>
                            <ProfileSelect opts=opts value=profile id="imp-profile" />
                        </div>
                    </div>
                    <div class="field-row">
                        {match kind {
                            MediaKind::Movie => view! {
                                <div class="field">
                                    <label for="imp-minavail">"Minimum availability"</label>
                                    <EnumSelect table=&MIN_AVAILABILITY value=min_avail id="imp-minavail" />
                                </div>
                            }.into_any(),
                            MediaKind::Series => view! {
                                <div class="field">
                                    <label for="imp-monitor">"Monitor"</label>
                                    <EnumSelect table=&SERIES_MONITOR value=monitor id="imp-monitor" />
                                </div>
                                <div class="field">
                                    <label for="imp-type">"Series type"</label>
                                    <EnumSelect table=&SERIES_TYPES value=series_type id="imp-type" />
                                </div>
                            }.into_any(),
                        }}
                    </div>
                    <div class="lib-toolbar">
                        <button type="submit" class="btn btn-secondary" disabled=move || root.get() == 0>
                            <Icon name="folder" />
                            "Scan folder"
                        </button>
                        <label class="checkbox">
                            <input type="checkbox" prop:checked=move || search_on_add.get()
                                on:change=move |e| search_on_add.set(event_target_checked(&e)) />
                            "Search for missing items after import"
                        </label>
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
                <span class="bulk-count">{move || if let Some((done, total)) = importing.get() {
                    format!("Importing… {done} of {total}")
                } else { match selected_count() {
                    0 => "Nothing selected: choose a match for a folder (or use Search) to import it".to_owned(),
                    n => format!("{n} ready to import"),
                }}}</span>
                <div class="bulk-actions">
                    <button type="button" class="btn btn-primary" disabled=move || busy.get() || selected_count() == 0 on:click=import>
                        {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="download" /> }.into_any() }}
                        "Import selected"
                    </button>
                </div>
            </div>
        </Show>

        <Modal open=search_open title="Find a match">
            <div class="modal-wide">
                <form class="lib-toolbar" on:submit=move |e: ev::SubmitEvent| { e.prevent_default(); search_term.set(search_input.get_untracked()); }>
                    <input class="input grow" type="search" aria-label="Search TMDB" autofocus
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
                            {list.into_iter().map(|r| {
                                let r2 = r.clone();
                                view! {
                                    <article class="lookup-item">
                                        <Poster title=r.title.clone() url=tmdb_image(r.poster.as_deref(), POSTER_SM) />
                                        <div class="overview-body">
                                            <span class="overview-title">{r.title.clone()}{r.year.map(|y| format!(" ({y})"))}</span>
                                            {r.overview.clone().map(|o| view! { <p class="overview-text">{o}</p> })}
                                        </div>
                                        <div>
                                            {if r.in_library {
                                                view! { <span class="badge tone-success">"In library"</span> }.into_any()
                                            } else {
                                                view! { <button type="button" class="btn btn-sm btn-primary" on:click=move |_| pick(r2.clone())>"Use"</button> }.into_any()
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
