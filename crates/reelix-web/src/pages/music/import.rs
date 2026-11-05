//! `/music/import` — scan a music root folder for artist folders that are not in the library,
//! match each to a MusicBrainz artist and add them (their files are scanned and linked).

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::library::{ImportErrorDto, LibraryImportMatchRequest};
use reelix_api_types::music::{
    ArtistLookupDto, ArtistMonitor, LibraryImportResult, MusicImportCandidateDto, MusicImportItem,
    MusicLibraryImportRequest,
};

use crate::api;
use crate::components::library_forms::{EnumSelect, enc, require_library};
use crate::components::music::{
    ARTIST_MONITOR, IdSelect, MusicOptions, metadata_items, profile_items, root_items,
};
use crate::components::{EmptyState, ErrorBox, Icon, PageHeader, Skeleton, Spinner};
use crate::state::Toasts;

#[derive(Clone)]
struct Row {
    folder: String,
    path: String,
    suggestions: RwSignal<Vec<ArtistLookupDto>>,
    /// Chosen MusicBrainz artist id (empty = skip).
    chosen: RwSignal<String>,
    selected: RwSignal<bool>,
    lookup: RwSignal<String>,
    searching: RwSignal<bool>,
    lookup_error: RwSignal<Option<String>>,
}

/// Folders sent per `POST /musicimport/match` (the server accepts up to 25; at MusicBrainz's
/// one request a second a batch takes about 10 s).
/// Folders added per import request.
const IMPORT_BATCH: usize = 25;
const MATCH_BATCH: usize = 10;

/// Best default: the first suggestion that is not in the library yet.
fn first_new(suggestions: &[ArtistLookupDto]) -> String {
    suggestions
        .iter()
        .find(|s| s.artist_id.is_none())
        .map(|s| s.musicbrainz_id.clone())
        .unwrap_or_default()
}

impl Row {
    fn new(c: MusicImportCandidateDto) -> Self {
        let first = first_new(&c.suggestions);
        Self {
            folder: c.folder,
            path: c.path,
            selected: RwSignal::new(!first.is_empty()),
            chosen: RwSignal::new(first),
            suggestions: RwSignal::new(c.suggestions),
            lookup: RwSignal::new(c.guessed_artist.unwrap_or_default()),
            searching: RwSignal::new(false),
            lookup_error: RwSignal::new(c.lookup_error),
        }
    }

    /// Fills in suggestions looked up after the scan, keeping an artist the user already picked.
    fn apply_match(&self, c: MusicImportCandidateDto) {
        let first = first_new(&c.suggestions);
        if self.chosen.get_untracked().is_empty() && !first.is_empty() {
            self.chosen.set(first);
            self.selected.set(true);
        }
        self.suggestions.update(|s| {
            for hit in c.suggestions {
                if !s.iter().any(|x| x.musicbrainz_id == hit.musicbrainz_id) {
                    s.push(hit);
                }
            }
        });
        self.lookup_error.set(c.lookup_error);
    }

    fn search(&self) {
        let toasts = Toasts::expect();
        let (term, suggestions, chosen, selected, searching) = (
            self.lookup.get_untracked(),
            self.suggestions,
            self.chosen,
            self.selected,
            self.searching,
        );
        if term.trim().len() < 2 {
            return;
        }
        searching.set(true);
        spawn_local(async move {
            match api::get::<Vec<ArtistLookupDto>>(&format!("/artists/lookup?term={}", enc(term.trim())))
                .await
            {
                Ok(list) => {
                    let first = list
                        .iter()
                        .find(|s| s.artist_id.is_none())
                        .map(|s| s.musicbrainz_id.clone());
                    suggestions.set(list);
                    if let Some(f) = first {
                        chosen.set(f);
                        selected.set(true);
                    }
                }
                Err(e) => toasts.error(format!("Lookup failed: {e}")),
            }
            searching.set(false);
        });
    }
}

/// Looks up MusicBrainz suggestions for `rows` in batches, updating them as batches arrive and
/// stopping once a newer scan (`scan_gen` moved on) replaced them.
async fn match_rows(
    root_folder_id: i64,
    rows: Vec<Row>,
    generation: u32,
    scan_gen: RwSignal<u32>,
    matching: RwSignal<Option<(usize, usize)>>,
) {
    let total = rows.len();
    matching.set((total > 0).then_some((0, total)));
    for (i, chunk) in rows.chunks(MATCH_BATCH).enumerate() {
        if scan_gen.get_untracked() != generation {
            return;
        }
        let req = LibraryImportMatchRequest {
            root_folder_id,
            paths: chunk.iter().map(|r| r.path.clone()).collect(),
        };
        match api::post::<_, Vec<MusicImportCandidateDto>>("/musicimport/match", &req).await {
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
}

#[component]
pub fn ImportArtistsPage() -> impl IntoView {
    require_library();
    let toasts = Toasts::expect();
    let opts = MusicOptions::load();
    let root = RwSignal::new(0i64);
    let profile = RwSignal::new(0i64);
    let metadata = RwSignal::new(0i64);
    let monitor = RwSignal::new(ArtistMonitor::Existing);
    let rows = RwSignal::new(None::<Result<Vec<Row>, api::ApiError>>);
    let scanning = RwSignal::new(false);
    let busy = RwSignal::new(false);
    // (done, total) while selected folders are being imported in batches.
    let importing = RwSignal::new(None::<(usize, usize)>);
    let result = RwSignal::new(None::<LibraryImportResult>);
    // Bumped on every scan (and import), so a stale scan or match batch is ignored.
    let scan_gen = RwSignal::new(0u32);
    // (done, total) while suggestions are being looked up after a scan.
    let matching = RwSignal::new(None::<(usize, usize)>);

    let scan = move |_| {
        let r = root.get_untracked();
        if r == 0 {
            toasts.error("Choose a music root folder first.");
            return;
        }
        scan_gen.update(|g| *g += 1);
        let generation = scan_gen.get_untracked();
        scanning.set(true);
        result.set(None);
        rows.set(None);
        matching.set(None);
        spawn_local(async move {
            // Folders only (fast); suggestions follow in batches.
            let res = api::get::<Vec<MusicImportCandidateDto>>(&format!(
                "/musicimport/scan?root_folder_id={r}&lookup=false"
            ))
            .await
            .map(|list| list.into_iter().map(Row::new).collect::<Vec<_>>());
            if scan_gen.get_untracked() != generation {
                return;
            }
            let pending = res.as_ref().ok().cloned();
            rows.set(Some(res));
            scanning.set(false);
            if let Some(list) = pending {
                match_rows(r, list, generation, scan_gen, matching).await;
            }
        });
    };
    let selected_count = move || match rows.get() {
        Some(Ok(list)) => list
            .iter()
            .filter(|r| r.selected.get() && !r.chosen.get().is_empty())
            .count(),
        _ => 0,
    };

    let import = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(Ok(list)) = rows.get_untracked() else {
            return;
        };
        let (p, m) = (profile.get_untracked(), metadata.get_untracked());
        if p == 0 || m == 0 {
            toasts.error("Choose a quality profile and a metadata profile.");
            return;
        }
        let items: Vec<MusicImportItem> = list
            .iter()
            .filter(|r| r.selected.get_untracked() && !r.chosen.get_untracked().is_empty())
            .map(|r| MusicImportItem {
                path: r.path.clone(),
                musicbrainz_id: r.chosen.get_untracked(),
                quality_profile_id: p,
                metadata_profile_id: m,
                monitor: monitor.get_untracked(),
                tags: Vec::new(),
            })
            .collect();
        if items.is_empty() {
            toasts.error("Select at least one folder with a matched artist.");
            return;
        }
        let root_folder_id = root.get_untracked();
        busy.set(true);
        spawn_local(async move {
            // Small batches keep every request short (a reverse proxy may cut long ones off).
            let total = items.len();
            let mut all = LibraryImportResult::default();
            importing.set(Some((0, total)));
            for chunk in items.chunks(IMPORT_BATCH) {
                let req = MusicLibraryImportRequest {
                    root_folder_id,
                    items: chunk.to_vec(),
                };
                match api::post::<_, LibraryImportResult>("/musicimport", &req).await {
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
                toasts.success(format!("Added {} artist(s).", all.added.len()));
            } else {
                toasts.error(format!(
                    "Imported {}; {} folder(s) could not be imported.",
                    all.added.len(),
                    all.errors.len()
                ));
            }
            result.set(Some(all));
            scan_gen.update(|g| *g += 1);
            matching.set(None);
            rows.set(None);
            busy.set(false);
        });
    };

    let table = move || {
        match rows.get() {
        None if scanning.get() => view! {
            <p class="muted"><Spinner />" Scanning folders…"</p>
            <Skeleton lines=6 />
        }.into_any(),
        None => view! {
            <div class="card">
                <EmptyState icon="folder" title="Import existing music"
                    description="Pick a music root folder and scan it. Every artist folder that is not in the library yet is listed with MusicBrainz suggestions; files inside are matched to albums by their tags and folder names." />
            </div>
        }
        .into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <div class="card">
                <EmptyState icon="check" title="Nothing to import" description="Every folder in this root folder already belongs to an artist." />
            </div>
        }
        .into_any(),
        Some(Ok(list)) => {
            let errs = list.clone();
            let lookup_error = move || errs.iter().find_map(|r| r.lookup_error.get());
            view! {
            {move || matching.get().map(|(done, total)| view! {
                <p class="muted"><Spinner />{format!(" Matching folders with MusicBrainz… {done} of {total}")}</p>
            })}
            {move || lookup_error().map(|e| view! {
                <div class="alert alert-warning" role="alert">
                    <Icon name="warning" />
                    <div class="alert-body">
                        <strong>"Folders couldn't be matched automatically"</strong>
                        <span>{e}</span>
                        <span>"You can still search MusicBrainz for each folder by hand."</span>
                    </div>
                </div>
            })}
            <form class="card" on:submit=import>
                <div class="card-body">
                    <div class="field-row">
                        <div class="field">
                            <label for="mi-profile">"Quality profile"</label>
                            <IdSelect items=profile_items(opts) value=profile id="mi-profile" />
                        </div>
                        <div class="field">
                            <label for="mi-meta">"Metadata profile"</label>
                            <IdSelect items=metadata_items(opts) value=metadata id="mi-meta" />
                        </div>
                        <div class="field">
                            <label for="mi-monitor">"Monitor"</label>
                            <EnumSelect table=&ARTIST_MONITOR value=monitor id="mi-monitor" />
                        </div>
                    </div>
                    <div class="table-wrap">
                        <table class="table">
                            <caption class="visually-hidden">"Folders to import"</caption>
                            <thead>
                                <tr>
                                    <th scope="col"><span class="visually-hidden">"Import"</span></th>
                                    <th scope="col">"Folder"</th>
                                    <th scope="col">"Artist"</th>
                                    <th scope="col">"Search MusicBrainz"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {list.into_iter().map(|r| {
                                    let r2 = r.clone();
                                    let r3 = r.clone();
                                    view! {
                                        <tr>
                                            <td>
                                                <input type="checkbox" aria-label=format!("Import {}", r.folder)
                                                    prop:checked=move || r.selected.get()
                                                    on:change=move |e| r.selected.set(event_target_checked(&e)) />
                                            </td>
                                            <td class="cell-strong">{r.folder.clone()}<span class="cell-sub mono">{r.path.clone()}</span></td>
                                            <td>
                                                {move || {
                                                    let s = r.suggestions.get();
                                                    if s.is_empty() {
                                                        return view! { <span class="muted">"No match"</span> }.into_any();
                                                    }
                                                    view! {
                                                        <select class="input" aria-label=format!("Artist for {}", r.folder)
                                                            on:change=move |e| r.chosen.set(event_target_value(&e))>
                                                            <option value="" selected=move || r.chosen.get().is_empty()>"— skip —"</option>
                                                            {s.into_iter().map(|a| {
                                                                let id = a.musicbrainz_id.clone();
                                                                let id2 = id.clone();
                                                                let label = match (&a.disambiguation, a.artist_id) {
                                                                    (_, Some(_)) => format!("{} (already in library)", a.name),
                                                                    (Some(d), None) => format!("{} ({d})", a.name),
                                                                    (None, None) => a.name.clone(),
                                                                };
                                                                view! { <option value=id disabled=a.artist_id.is_some() selected=move || r.chosen.get() == id2>{label}</option> }
                                                            }).collect_view()}
                                                        </select>
                                                    }.into_any()
                                                }}
                                            </td>
                                            <td>
                                                <div class="meta-line">
                                                    <input class="input" type="search" aria-label=format!("Search for {}", r2.folder)
                                                        prop:value=move || r2.lookup.get() on:input=move |e| r2.lookup.set(event_target_value(&e)) />
                                                    <button type="button" class="icon-btn" title="Search" aria-label="Search"
                                                        on:click=move |_| r3.search()>
                                                        {move || if r3.searching.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="search" /> }.into_any() }}
                                                    </button>
                                                </div>
                                            </td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                    <div class="modal-footer">
                        <span class="muted">{move || if let Some((done, total)) = importing.get() { format!("Importing… {done} of {total}") } else { match selected_count() {
                            0 => "Nothing selected: choose an artist for a folder (or use Search) to import it".to_owned(),
                            n => format!("{n} ready to import"),
                        }}}</span>
                        <button type="submit" class="btn btn-primary" disabled=move || busy.get() || selected_count() == 0>
                            {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="plus" /> }.into_any() }}
                            "Import selected"
                        </button>
                    </div>
                </div>
            </form>
            }
            .into_any()
        }
    }
    };

    view! {
        <PageHeader title="Import music" subtitle="Add artist folders you already have; their files are scanned and matched to albums." />
        <div class="lib-toolbar">
            <label for="mi-root" class="visually-hidden">"Root folder"</label>
            <IdSelect items=root_items(opts) value=root id="mi-root" />
            <button type="button" class="btn btn-secondary" disabled=move || scanning.get() on:click=scan>
                {move || if scanning.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="folder" /> }.into_any() }}
                "Scan folder"
            </button>
        </div>
        {move || result.get().map(|r| view! {
            <div class="card">
                <div class="card-body">
                    <p>{format!("Added {} artist(s); files are being matched to albums.", r.added.len())}</p>
                    {(!r.errors.is_empty()).then(|| view! {
                        <ul class="stack">
                            {r.errors.iter().map(|e| view! { <li class="text-danger"><span class="mono">{e.path.clone()}</span>": "{e.message.clone()}</li> }).collect_view()}
                        </ul>
                    })}
                </div>
            </div>
        })}
        {table}
    }
}
