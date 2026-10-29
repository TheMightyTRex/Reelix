//! Manual import modal: scan a folder (or a queue item's download), review the detected media,
//! episodes, quality and language of each file, fix mismatches and import the selection.

use std::collections::HashMap;

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::library::{EpisodeDto, MovieDto, Paged, SeriesDto};
use reelix_api_types::pipeline::{ManualImportFile, ManualImportItemDto, ManualImportRequest};
use reelix_parser::{Language, Quality, QualityModel};

use super::history_table::{episode_code, quality_label};
use super::library_common::bytes;
use super::library_forms::with_query;
use super::{EmptyState, ErrorBox, Icon, Modal, Spinner};
use crate::api::{self, ApiError};
use crate::state::Toasts;

/// Where to look for files.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManualImportSource {
    /// Folder to scan (editable in the dialog).
    pub folder: Option<String>,
    /// A queue item's download id (scans the client's output folder).
    pub download_id: Option<String>,
    /// Restrict matching to this movie / series.
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    /// Shown under the dialog title (e.g. the release name).
    pub label: Option<String>,
}

/// Encodes a media choice for a `<select>`: `movie:1`, `series:2` or `""`.
fn media_key(kind: &str, id: i64) -> String {
    format!("{kind}:{id}")
}

fn parse_media_key(key: &str) -> Option<(&str, i64)> {
    let (kind, id) = key.split_once(':')?;
    Some((kind, id.parse().ok()?))
}

/// One editable file row.
#[derive(Clone, Copy)]
struct Row {
    item: StoredValue<ManualImportItemDto>,
    selected: RwSignal<bool>,
    media: RwSignal<String>,
    season: RwSignal<Option<u32>>,
    episodes: RwSignal<Vec<i64>>,
    quality: RwSignal<QualityModel>,
    languages: RwSignal<Vec<Language>>,
    release_group: RwSignal<String>,
}

impl Row {
    fn new(item: ManualImportItemDto) -> Self {
        let media = item
            .media
            .as_ref()
            .map(|m| media_key(&m.kind, m.id))
            .unwrap_or_default();
        let season = item
            .season_number
            .or_else(|| item.episodes.first().map(|e| e.season_number));
        let ok = item.rejections.is_empty() && item.media.is_some();
        Self {
            selected: RwSignal::new(ok),
            media: RwSignal::new(media),
            season: RwSignal::new(season),
            episodes: RwSignal::new(item.episodes.iter().map(|e| e.id).collect()),
            quality: RwSignal::new(item.quality),
            languages: RwSignal::new(item.languages.clone()),
            release_group: RwSignal::new(item.release_group.clone().unwrap_or_default()),
            item: StoredValue::new(item),
        }
    }

    /// Why this row can't be submitted yet, if anything.
    fn problem(&self) -> Option<&'static str> {
        match parse_media_key(&self.media.get()) {
            None => Some("Pick a movie or series"),
            Some(("series", _)) if self.episodes.with(Vec::is_empty) => Some("Pick the episode(s)"),
            _ => None,
        }
    }

    fn to_file(self) -> ManualImportFile {
        let item = self.item.get_value();
        let (movie_id, series_id) = match parse_media_key(&self.media.get_untracked()) {
            Some(("movie", id)) => (Some(id), None),
            Some(("series", id)) => (None, Some(id)),
            _ => (None, None),
        };
        let group = self.release_group.get_untracked();
        ManualImportFile {
            path: item.path,
            movie_id,
            series_id,
            episode_ids: if series_id.is_some() {
                self.episodes.get_untracked()
            } else {
                Vec::new()
            },
            quality: Some(self.quality.get_untracked()),
            languages: self.languages.get_untracked(),
            release_group: (!group.trim().is_empty()).then(|| group.trim().to_string()),
            download_id: item.download_id,
        }
    }
}

/// The dialog. Set `source` then `open`.
#[component]
pub fn ManualImportModal(
    open: RwSignal<bool>,
    source: RwSignal<Option<ManualImportSource>>,
) -> impl IntoView {
    view! {
        <Modal open=open title="Manual import">
            <ManualImportBody open=open source=source.get_untracked().unwrap_or_default() />
        </Modal>
    }
}

#[component]
fn ManualImportBody(open: RwSignal<bool>, source: ManualImportSource) -> impl IntoView {
    let toasts = Toasts::expect();
    let folder = RwSignal::new(source.folder.clone().unwrap_or_default());
    let download_id = source.download_id.clone();
    let has_download = download_id.is_some();
    let scanning = RwSignal::new(false);
    let rows = RwSignal::new(None::<Result<Vec<Row>, ApiError>>);
    let mode = RwSignal::new("auto".to_string());
    let submitting = RwSignal::new(false);

    let movies = LocalResource::new(|| api::get::<Paged<MovieDto>>("/movies?sort=title&dir=asc"));
    let series = LocalResource::new(|| api::get::<Paged<SeriesDto>>("/series?sort=title&dir=asc"));
    let episode_cache = RwSignal::new(HashMap::<i64, Vec<EpisodeDto>>::new());
    let ensure_episodes = move |series_id: i64| {
        if episode_cache.with_untracked(|c| c.contains_key(&series_id)) {
            return;
        }
        episode_cache.update(|c| {
            c.insert(series_id, Vec::new());
        });
        spawn_local(async move {
            match api::get::<Vec<EpisodeDto>>(&format!("/series/{series_id}/episodes")).await {
                Ok(mut eps) => {
                    eps.sort_by_key(|e| (e.season_number, e.episode_number));
                    let _ = episode_cache.try_update(|c| {
                        c.insert(series_id, eps);
                    });
                }
                Err(e) => toasts.error(format!("Could not load episodes: {e}")),
            }
        });
    };

    let movie_id = source.movie_id;
    let series_id = source.series_id;
    let scan = move || {
        let dl = download_id.clone();
        let f = folder.get_untracked();
        if dl.is_none() && f.trim().is_empty() {
            return;
        }
        let url = with_query(
            "/manualimport",
            &[
                (
                    "folder",
                    if dl.is_some() {
                        String::new()
                    } else {
                        f.trim().to_string()
                    },
                ),
                ("download_id", dl.unwrap_or_default()),
                ("movie_id", movie_id.map(|v| v.to_string()).unwrap_or_default()),
                ("series_id", series_id.map(|v| v.to_string()).unwrap_or_default()),
            ],
        );
        scanning.set(true);
        rows.set(None);
        spawn_local(async move {
            let r = api::get::<Vec<ManualImportItemDto>>(&url)
                .await
                .map(|items| items.into_iter().map(Row::new).collect::<Vec<_>>());
            if let Ok(list) = &r {
                for row in list {
                    if let Some(("series", sid)) = parse_media_key(&row.media.get_untracked()) {
                        ensure_episodes(sid);
                    }
                }
            }
            let _ = rows.try_set(Some(r));
            let _ = scanning.try_set(false);
        });
    };
    let scan = StoredValue::new(scan);

    // A queue item or a prefilled folder scans right away.
    if has_download || source.folder.as_deref().is_some_and(|f| !f.is_empty()) {
        scan.with_value(|s| s());
    }

    let on_scan = move |e: ev::SubmitEvent| {
        e.prevent_default();
        scan.with_value(|s| s());
    };

    let selected_rows = move || -> Vec<Row> {
        rows.with(|r| match r {
            Some(Ok(list)) => list.iter().copied().filter(|r| r.selected.get()).collect(),
            _ => Vec::new(),
        })
    };
    let blocked = move || selected_rows().iter().any(|r| r.problem().is_some());

    let submit = move |_| {
        let files: Vec<ManualImportFile> = selected_rows().into_iter().map(Row::to_file).collect();
        if files.is_empty() {
            return;
        }
        let n = files.len();
        let req = ManualImportRequest {
            files,
            import_mode: mode.get_untracked(),
        };
        submitting.set(true);
        spawn_local(async move {
            match api::post::<_, EnqueuedJob>("/manualimport", &req).await {
                Ok(_) => {
                    toasts.success(format!("Importing {n} file(s)…"));
                    open.set(false);
                }
                Err(e) => toasts.error(format!("Import failed: {e}")),
            }
            let _ = submitting.try_set(false);
        });
    };

    let media_options = move || {
        let mut out: Vec<(String, String)> = Vec::new();
        if let Some(Ok(p)) = movies.get() {
            out.extend(p.items.into_iter().map(|m| {
                let label = match m.year {
                    Some(y) => format!("{} ({y})", m.title),
                    None => m.title,
                };
                (media_key("movie", m.id), format!("Movie · {label}"))
            }));
        }
        if let Some(Ok(p)) = series.get() {
            out.extend(p.items.into_iter().map(|s| {
                let label = match s.year {
                    Some(y) => format!("{} ({y})", s.title),
                    None => s.title,
                };
                (media_key("series", s.id), format!("Series · {label}"))
            }));
        }
        out
    };
    let media_options = Memo::new(move |_| media_options());
    let seasons_of = move |sid: i64| -> Vec<u32> {
        series
            .get()
            .and_then(Result::ok)
            .and_then(|p| p.items.into_iter().find(|s| s.id == sid))
            .map(|s| s.seasons.iter().map(|x| x.season_number).collect())
            .unwrap_or_default()
    };

    let table = move || {
        if scanning.get() {
            return view! { <div class="isearch-loading"><Spinner label="Scanning files…" /></div> }
                .into_any();
        }
        match rows.get() {
            None => ().into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
            Some(Ok(list)) if list.is_empty() => view! {
                <EmptyState icon="folder" title="No video files found" compact=true
                    description="The folder has no importable video files (samples and extras are skipped)." />
            }
            .into_any(),
            Some(Ok(list)) => {
                let all = list.clone();
                view! {
                    <div class="table-wrap mi-table">
                        <table class="table">
                            <caption class="visually-hidden">"Files to import"</caption>
                            <thead>
                                <tr>
                                    <th scope="col" class="col-check">
                                        <input type="checkbox" aria-label="Select all"
                                            prop:checked=move || all.iter().all(|r| r.selected.get())
                                            on:change=move |e| { let on = event_target_checked(&e); for r in &list { r.selected.set(on); } } />
                                    </th>
                                    <th scope="col">"File"</th>
                                    <th scope="col">"Media"</th>
                                    <th scope="col">"Episodes"</th>
                                    <th scope="col">"Quality"</th>
                                    <th scope="col">"Language"</th>
                                    <th scope="col">"Group"</th>
                                    <th scope="col">"Size"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {rows.get_untracked().and_then(Result::ok).unwrap_or_default().into_iter().map(|row| {
                                    file_row(row, media_options, seasons_of, episode_cache, ensure_episodes)
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
        <div class="mi">
            {source.label.clone().map(|l| view! { <p class="modal-subtitle mono">{l}</p> })}
            {(!has_download).then(|| view! {
                <form class="field-row mi-folder" on:submit=on_scan>
                    <div class="field grow">
                        <label for="mi-folder">"Folder"</label>
                        <input id="mi-folder" class="input mono" type="text" placeholder="/downloads/complete/Movie.2024.1080p"
                            prop:value=move || folder.get() on:input=move |e| folder.set(event_target_value(&e)) />
                    </div>
                    <div class="field mi-folder-btn">
                        <button type="submit" class="btn btn-secondary" disabled=move || scanning.get() || folder.with(|f| f.trim().is_empty())>
                            <Icon name="search" />
                            "Scan"
                        </button>
                    </div>
                </form>
            })}
            {table}
            <Show when=move || matches!(rows.get(), Some(Ok(ref l)) if !l.is_empty())>
                <div class="modal-footer mi-footer">
                    <div class="field mi-mode">
                        <label for="mi-mode">"Import mode"</label>
                        <select id="mi-mode" class="input input-sm" on:change=move |e| mode.set(event_target_value(&e))
                            prop:value=move || mode.get()>
                            <option value="auto">"Automatic (settings)"</option>
                            <option value="move">"Move"</option>
                            <option value="copy">"Copy"</option>
                            <option value="hardlink">"Hardlink or copy"</option>
                        </select>
                    </div>
                    <span class="muted mi-summary">
                        {move || {
                            let sel = selected_rows();
                            match sel.iter().find_map(|r| r.problem()) {
                                Some(p) => format!("{} selected · {p}", sel.len()),
                                None => format!("{} selected", sel.len()),
                            }
                        }}
                    </span>
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="button" class="btn btn-primary"
                        disabled=move || submitting.get() || selected_rows().is_empty() || blocked()
                        on:click=submit>
                        <Icon name="download" />
                        "Import"
                    </button>
                </div>
            </Show>
        </div>
    }
}

fn file_row(
    row: Row,
    media_options: Memo<Vec<(String, String)>>,
    seasons_of: impl Fn(i64) -> Vec<u32> + Copy + Send + Sync + 'static,
    episode_cache: RwSignal<HashMap<i64, Vec<EpisodeDto>>>,
    ensure_episodes: impl Fn(i64) + Copy + 'static,
) -> impl IntoView {
    let item = row.item.get_value();
    let rejections = item.rejections.clone();
    let series_id = move || match parse_media_key(&row.media.get()) {
        Some(("series", id)) => Some(id),
        _ => None,
    };
    let path_title = item.path.clone();

    let episodes_cell = move || {
        let Some(sid) = series_id() else {
            return view! { <span class="muted">"—"</span> }.into_any();
        };
        let seasons = seasons_of(sid);
        let season = row.season.get();
        let eps: Vec<EpisodeDto> = episode_cache.with(|c| {
            c.get(&sid)
                .map(|v| {
                    v.iter()
                        .filter(|e| Some(e.season_number) == season)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        });
        let picked = row.episodes.get();
        let summary = if picked.is_empty() {
            "Pick…".to_string()
        } else {
            episode_cache.with(|c| {
                c.get(&sid)
                    .map(|v| {
                        v.iter()
                            .filter(|e| picked.contains(&e.id))
                            .map(|e| format!("S{:02}E{:02}", e.season_number, e.episode_number))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{} episode(s)", picked.len()))
            })
        };
        view! {
            <div class="mi-episodes">
                <select class="input input-sm" aria-label="Season"
                    on:change=move |e| {
                        row.season.set(event_target_value(&e).parse().ok());
                        row.episodes.set(Vec::new());
                    }>
                    <option value="" selected=season.is_none()>"Season…"</option>
                    {seasons.into_iter().map(|n| view! {
                        <option value=n.to_string() selected=season == Some(n)>
                            {if n == 0 { "Specials".to_string() } else { format!("Season {n}") }}
                        </option>
                    }).collect_view()}
                </select>
                <details class="mi-ep-picker">
                    <summary class="input input-sm">{summary}</summary>
                    <div class="mi-ep-list">
                        {if eps.is_empty() {
                            view! { <span class="muted">"No episodes"</span> }.into_any()
                        } else {
                            eps.into_iter().map(|e| {
                                let eid = e.id;
                                let label = format!(
                                    "{}{}",
                                    episode_code(&reelix_api_types::pipeline::EpisodeRefDto {
                                        id: e.id,
                                        season_number: e.season_number,
                                        episode_number: e.episode_number,
                                        title: None,
                                    }),
                                    e.title.as_ref().map(|t| format!(" · {t}")).unwrap_or_default()
                                );
                                view! {
                                    <label class="checkbox">
                                        <input type="checkbox" prop:checked=move || row.episodes.with(|v| v.contains(&eid))
                                            on:change=move |ev| {
                                                let on = event_target_checked(&ev);
                                                row.episodes.update(|v| { v.retain(|x| *x != eid); if on { v.push(eid); } });
                                            } />
                                        {label}
                                    </label>
                                }
                            }).collect_view().into_any()
                        }}
                    </div>
                </details>
            </div>
        }
        .into_any()
    };

    view! {
        <tr class:is-selected=move || row.selected.get()>
            <td class="col-check">
                <input type="checkbox" aria-label="Import this file" prop:checked=move || row.selected.get()
                    on:change=move |e| row.selected.set(event_target_checked(&e)) />
            </td>
            <td class="mi-file" title=path_title>
                <span class="mono file-name">{item.relative_path.clone()}</span>
                {(!rejections.is_empty()).then(|| view! {
                    <ul class="reject-list">
                        {rejections.iter().map(|r| view! { <li>{r.clone()}</li> }).collect_view()}
                    </ul>
                })}
            </td>
            <td>
                <select class="input input-sm mi-media" aria-label="Media"
                    on:change=move |e| {
                        let v = event_target_value(&e);
                        if let Some(("series", sid)) = parse_media_key(&v) {
                            ensure_episodes(sid);
                        }
                        row.media.set(v);
                        row.episodes.set(Vec::new());
                        row.selected.set(true);
                    }>
                    <option value="" selected=move || row.media.with(String::is_empty)>"Unknown — pick…"</option>
                    {move || media_options.get().into_iter().map(|(k, l)| {
                        let k2 = k.clone();
                        view! { <option value=k selected=move || row.media.with(|m| *m == k2)>{l}</option> }
                    }).collect_view()}
                </select>
            </td>
            <td>{episodes_cell}</td>
            <td>
                <select class="input input-sm" aria-label="Quality"
                    on:change=move |e| {
                        if let Some(q) = event_target_value(&e).parse::<u16>().ok().and_then(Quality::from_id) {
                            row.quality.update(|m| m.quality = q);
                        }
                    }>
                    {Quality::ALL.iter().map(|q| {
                        let id = q.id();
                        view! { <option value=id.to_string() selected=move || row.quality.with(|m| m.quality.id() == id)>{q.name()}</option> }
                    }).collect_view()}
                </select>
                <span class="cell-sub">{move || quality_label(&row.quality.get())}</span>
            </td>
            <td>
                <select class="input input-sm" aria-label="Language"
                    on:change=move |e| {
                        if let Some(l) = event_target_value(&e).parse::<u16>().ok().and_then(Language::from_id) {
                            row.languages.set(vec![l]);
                        }
                    }>
                    {Language::ALL.iter().map(|l| {
                        let id = l.id();
                        view! { <option value=id.to_string() selected=move || row.languages.with(|v| v.first().map(|x| x.id()) == Some(id))>{l.name()}</option> }
                    }).collect_view()}
                </select>
            </td>
            <td>
                <input class="input input-sm mi-group" type="text" aria-label="Release group"
                    prop:value=move || row.release_group.get() on:input=move |e| row.release_group.set(event_target_value(&e)) />
            </td>
            <td class="nowrap">{bytes(item.size)}</td>
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_keys_round_trip() {
        assert_eq!(parse_media_key(&media_key("series", 7)), Some(("series", 7)));
        assert_eq!(parse_media_key(""), None);
        assert_eq!(parse_media_key("movie:x"), None);
    }
}
