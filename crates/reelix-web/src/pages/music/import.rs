//! `/music/import` — scan a music root folder for artist folders that are not in the library,
//! match each to a MusicBrainz artist and add them (their files are scanned and linked).

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
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
}

impl Row {
    fn new(c: MusicImportCandidateDto) -> Self {
        let first = c
            .suggestions
            .iter()
            .find(|s| s.artist_id.is_none())
            .map(|s| s.musicbrainz_id.clone())
            .unwrap_or_default();
        Self {
            folder: c.folder,
            path: c.path,
            selected: RwSignal::new(!first.is_empty()),
            chosen: RwSignal::new(first),
            suggestions: RwSignal::new(c.suggestions),
            lookup: RwSignal::new(c.guessed_artist.unwrap_or_default()),
            searching: RwSignal::new(false),
        }
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
    let result = RwSignal::new(None::<LibraryImportResult>);

    let scan = move |_| {
        let r = root.get_untracked();
        if r == 0 {
            toasts.error("Choose a music root folder first.");
            return;
        }
        scanning.set(true);
        result.set(None);
        spawn_local(async move {
            let res =
                api::get::<Vec<MusicImportCandidateDto>>(&format!("/musicimport/scan?root_folder_id={r}"))
                    .await;
            rows.set(Some(res.map(|list| list.into_iter().map(Row::new).collect())));
            scanning.set(false);
        });
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
        let req = MusicLibraryImportRequest {
            root_folder_id: root.get_untracked(),
            items,
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, LibraryImportResult>("/musicimport", &req).await {
                Ok(r) => {
                    toasts.success(format!("Added {} artist(s).", r.added.len()));
                    result.set(Some(r));
                    rows.set(None);
                }
                Err(e) => toasts.error(format!("Import failed: {e}")),
            }
            busy.set(false);
        });
    };

    let table = move || {
        match rows.get() {
        None if scanning.get() => view! { <Skeleton lines=6 /> }.into_any(),
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
        Some(Ok(list)) => view! {
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
                        <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                            {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="plus" /> }.into_any() }}
                            "Import selected"
                        </button>
                    </div>
                </div>
            </form>
        }
        .into_any(),
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
