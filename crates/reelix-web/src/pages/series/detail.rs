//! `/series/{id}` — manage one series: header, seasons accordion with episodes, files, cast.

use chrono::Utc;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};
use reelix_api_types::library::{
    EpisodeDto, EpisodeFileDto, EpisodeMonitorRequest, LibrarySearchKind, MediaKind, SeasonDto,
    SeasonMonitorRequest, SeasonPassRequest, SeriesDto, SeriesEdit, SeriesMonitor,
};

use super::library::series_state;
use crate::api;
use crate::components::history_table::HistoryTable;
use crate::components::interactive_search::{InteractiveSearchModal, SearchTarget};
use crate::components::library_bulk::{EditFields, EditMediaModal};
use crate::components::library_common::{LibIcon, bytes, on_entity_changed, runtime};
use crate::components::library_forms::{
    DeleteMediaDialog, EnumSelect, LibOptions, RenameModal, SERIES_MONITOR, SERIES_TYPES, fire, opt_label,
    require_library, series_status_label, start_search,
};
use crate::components::library_media::{CastRow, MediaFilesTable};
use crate::components::manual_import::{ManualImportModal, ManualImportSource};
use crate::components::poster::{BACKDROP, EpisodeProgress, POSTER_MD, Poster, tmdb_image};
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, Modal, Skeleton, StatusBadge, Tone};
use crate::state::Toasts;

#[component]
pub fn SeriesDetailPage() -> impl IntoView {
    require_library();
    let params = use_params_map();
    let id = move || params.with(|p| p.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0));
    view! { {move || view! { <SeriesDetail id=id() /> }} }
}

fn episode_code(season: u32, ep: u32) -> String {
    format!("S{season:02}E{ep:02}")
}

/// Status cell for an episode: quality when downloaded, else unaired / missing / unmonitored.
fn episode_status(e: &EpisodeDto) -> (Tone, String) {
    if let Some(f) = &e.episode_file {
        let tone = if f.file.cutoff_met {
            Tone::Success
        } else {
            Tone::Warning
        };
        return (tone, f.file.quality_name.clone());
    }
    if e.has_file {
        return (Tone::Success, "Downloaded".into());
    }
    let today = Utc::now().date_naive();
    match e.air_date {
        None => (Tone::Neutral, "TBA".into()),
        Some(d) if d > today => (Tone::Info, "Unaired".into()),
        Some(_) if e.monitored => (Tone::Danger, "Missing".into()),
        Some(_) => (Tone::Neutral, "Unmonitored".into()),
    }
}

#[component]
fn MonitorToggle(monitored: bool, #[prop(into)] label: String, on_toggle: Callback<()>) -> impl IntoView {
    let text = format!("{} {label}", if monitored { "Unmonitor" } else { "Monitor" });
    view! {
        <button type="button" class="monitor-btn" class:off=!monitored aria-pressed=monitored.to_string()
            title=text.clone() aria-label=text on:click=move |e| { e.stop_propagation(); on_toggle.run(()); }>
            <LibIcon name=if monitored { "bookmark" } else { "bookmark-off" } />
        </button>
    }
}

#[component]
fn SeriesDetail(id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let opts = LibOptions::load();
    let series = LocalResource::new(move || {
        let url = format!("/series/{id}");
        async move { api::get::<SeriesDto>(&url).await }
    });
    let episodes = LocalResource::new(move || {
        let url = format!("/series/{id}/episodes");
        async move { api::get::<Vec<EpisodeDto>>(&url).await }
    });
    let files = LocalResource::new(move || {
        let url = format!("/episodefiles?series_id={id}");
        async move { api::get::<Vec<EpisodeFileDto>>(&url).await }
    });
    on_entity_changed(
        &["series", "season", "episode", "episode_file", "episodefile"],
        true,
        move || {
            series.refetch();
            episodes.refetch();
            files.refetch();
        },
    );

    let tab = RwSignal::new("seasons");
    // Seasons the user opened; `None` = default (the newest regular season).
    let open_seasons = RwSignal::new(None::<Vec<u32>>);
    let edit_open = RwSignal::new(false);
    let fields = EditFields::new();
    let original_path = RwSignal::new(String::new());
    let delete_open = RwSignal::new(false);
    let rename_open = RwSignal::new(false);
    let pass_open = RwSignal::new(false);
    let pass_monitor = RwSignal::new(SeriesMonitor::All);
    let file_delete = RwSignal::new(None::<i64>);
    let file_delete_open = RwSignal::new(false);
    let isearch_open = RwSignal::new(false);
    let isearch_target = RwSignal::new(None::<SearchTarget>);
    let mi_open = RwSignal::new(false);
    let mi_source = RwSignal::new(None::<ManualImportSource>);
    let series_title = move || {
        series
            .get_untracked()
            .and_then(Result::ok)
            .map(|s| s.title)
            .unwrap_or_default()
    };
    let interactive = move |t: SearchTarget| {
        isearch_target.set(Some(t));
        isearch_open.set(true);
    };

    let refresh_all = move || {
        series.refetch();
        episodes.refetch();
    };

    let open_edit = move |s: &SeriesDto| {
        fields.monitored.set(s.monitored);
        fields.profile.set(s.quality_profile_id);
        fields.series_type.set(s.series_type);
        fields.season_folder.set(s.season_folder);
        fields.path.set(s.path.clone());
        fields.move_files.set(false);
        fields.tags.set(s.tags.clone());
        original_path.set(s.path.clone());
        edit_open.set(true);
    };

    let toggle_series = move |monitored: bool| {
        let body = SeriesEdit {
            monitored: Some(!monitored),
            ..Default::default()
        };
        spawn_local(async move {
            match api::put::<_, serde_json::Value>(&format!("/series/{id}"), &body).await {
                Ok(_) => series.refetch(),
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };
    let toggle_season = move |season_number: u32, monitored: bool| {
        let body = SeasonMonitorRequest {
            series_id: id,
            season_number,
            monitored: !monitored,
            include_episodes: true,
        };
        spawn_local(async move {
            match api::put::<_, serde_json::Value>("/seasons/monitor", &body).await {
                Ok(_) => refresh_all(),
                Err(e) => toasts.error(e.to_string()),
            }
        });
    };
    let toggle_episodes = move |episode_ids: Vec<i64>, monitored: bool| {
        // Optimistic update so toggles feel instant.
        episodes.update(|r| {
            if let Some(Ok(list)) = r.as_mut() {
                for e in list.iter_mut().filter(|e| episode_ids.contains(&e.id)) {
                    e.monitored = monitored;
                }
            }
        });
        let body = EpisodeMonitorRequest {
            episode_ids,
            monitored,
        };
        spawn_local(async move {
            match api::put::<_, serde_json::Value>("/episodes/monitor", &body).await {
                Ok(_) => series.refetch(),
                Err(e) => {
                    toasts.error(e.to_string());
                    episodes.refetch();
                }
            }
        });
    };
    let season_pass = move |_| {
        let req = SeasonPassRequest {
            series_ids: vec![id],
            monitor: pass_monitor.get_untracked(),
            monitored: None,
        };
        spawn_local(async move {
            match api::post::<_, serde_json::Value>("/seasonpass", &req).await {
                Ok(_) => {
                    toasts.success("Monitoring updated.");
                    pass_open.set(false);
                    refresh_all();
                }
                Err(e) => toasts.error(format!("Could not apply: {e}")),
            }
        });
    };
    let delete = Callback::new(move |delete_files: bool| {
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::delete(&format!("/series/{id}?delete_files={delete_files}")).await {
                Ok(()) => {
                    toasts.success("Series deleted.");
                    navigate("/series", Default::default());
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
            match api::delete(&format!("/episodefiles/{fid}")).await {
                Ok(()) => {
                    toasts.success("File deleted.");
                    files.refetch();
                    refresh_all();
                }
                Err(e) => toasts.error(format!("Could not delete the file: {e}")),
            }
        });
    });

    let header = move || {
        let s = match series.get() {
            None => return view! { <div class="detail-hero"><Skeleton lines=5 /></div> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| series.refetch()) /> }
                    .into_any();
            }
            Some(Ok(s)) => s,
        };
        let state = series_state(&s);
        let st = s.statistics.clone();
        let backdrop = tmdb_image(s.images.backdrop.as_deref(), BACKDROP)
            .map(|u| format!("background-image: url('{}')", u.replace('\'', "%27")))
            .unwrap_or_default();
        let s_edit = s.clone();
        let monitored = s.monitored;
        let title = s.title.clone();
        let tags = opts.tag_names(&s.tags);
        let newest_season = s.seasons.iter().map(|x| x.season_number).filter(|n| *n > 0).max();
        view! {
            <div class="detail-hero">
                <div class="detail-backdrop" style=backdrop></div>
                <div class="detail-inner">
                    <Poster title=s.title.clone() url=tmdb_image(s.images.poster.as_deref(), POSTER_MD) />
                    <div class="detail-info">
                        <h1 class="detail-title">
                            {s.title.clone()}
                            {s.year.map(|y| view! { <span class="year">{format!(" ({y})")}</span> })}
                        </h1>
                        <div class="meta-line">
                            <StatusBadge tone=state.tone() label=state.label() dot=true />
                            <span>{series_status_label(s.status)}</span>
                            {s.network.clone().map(|n| view! { <span>{n}</span> })}
                            {s.runtime_minutes.filter(|r| *r > 0).map(|r| view! { <span>{runtime(r)}</span> })}
                            {s.certification.clone().map(|c| view! { <span class="chip">{c}</span> })}
                            {(s.ratings.vote_average > 0.0).then(|| view! { <span class="rating"><Icon name="star" class="icon-fill" />{format!("{:.1}", s.ratings.vote_average)}</span> })}
                            {s.genres.iter().take(4).map(|g| view! { <span class="chip">{g.clone()}</span> }).collect_view()}
                        </div>
                        {s.overview.clone().map(|o| view! { <p class="detail-overview">{o}</p> })}
                        <div class="detail-facts">
                            <div class="fact"><span class="fact-label">"Path"</span><span class="mono">{s.path.clone()}</span></div>
                            <div class="fact"><span class="fact-label">"Profile"</span><span>{opts.profile_name(s.quality_profile_id)}</span></div>
                            <div class="fact"><span class="fact-label">"Type"</span><span>{opt_label(&SERIES_TYPES, s.series_type)}</span></div>
                            <div class="fact"><span class="fact-label">"Size"</span><span>{bytes(st.size_on_disk)}</span></div>
                            <div class="fact"><span class="fact-label">"Episodes"</span><EpisodeProgress have=st.episode_file_count total=st.episode_count /></div>
                            {s.next_airing.map(|d| view! {
                                <div class="fact"><span class="fact-label">"Next airing"</span><span>{d.format("%Y-%m-%d").to_string()}</span></div>
                            })}
                            {(!tags.is_empty()).then(|| view! {
                                <div class="fact"><span class="fact-label">"Tags"</span>
                                    <span class="chip-list">{tags.into_iter().map(|t| view! { <span class="chip">{t}</span> }).collect_view()}</span>
                                </div>
                            })}
                        </div>
                        <div class="detail-actions">
                            <button type="button" class="btn btn-primary"
                                on:click=move |_| start_search(LibrarySearchKind::Series, vec![id], None, None, title.clone())>
                                <Icon name="search" />
                                "Search monitored"
                            </button>
                            <button type="button" class="btn btn-secondary" disabled=newest_season.is_none()
                                title="Interactive search for the latest season"
                                on:click=move |_| if let Some(n) = newest_season {
                                    interactive(SearchTarget::season(id, n, format!("{} · Season {n}", series_title())));
                                }>
                                <Icon name="list" />
                                "Interactive"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| {
                                    mi_source.set(Some(ManualImportSource { series_id: Some(id), ..Default::default() }));
                                    mi_open.set(true);
                                }>
                                <Icon name="download" />
                                "Manual import"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| fire(format!("/series/{id}/refresh"), "Refresh queued.".into())>
                                <Icon name="refresh" />
                                "Refresh"
                            </button>
                            <button type="button" class="btn btn-secondary"
                                on:click=move |_| fire(format!("/series/{id}/rescan"), "Disk scan queued.".into())>
                                <Icon name="folder" />
                                "Rescan"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| rename_open.set(true)>
                                <LibIcon name="rename" />
                                "Rename"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| pass_open.set(true)>
                                <LibIcon name="bookmark" />
                                "Season pass"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| toggle_series(monitored)>
                                <LibIcon name=if monitored { "bookmark-off" } else { "bookmark" } />
                                {if monitored { "Unmonitor" } else { "Monitor" }}
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| open_edit(&s_edit)>
                                <LibIcon name="edit" />
                                "Edit"
                            </button>
                            <button type="button" class="btn btn-ghost btn-danger-text" on:click=move |_| delete_open.set(true)>
                                <Icon name="trash" />
                                "Delete"
                            </button>
                            {s.imdb_id.clone().map(|i| view! {
                                <a class="btn btn-ghost" href=format!("https://www.imdb.com/title/{i}/") target="_blank" rel="noopener noreferrer"><Icon name="external" />"IMDb"</a>
                            })}
                        </div>
                    </div>
                </div>
            </div>
        }
        .into_any()
    };

    let season_block = move |season: SeasonDto, eps: Vec<EpisodeDto>, default_open: bool| {
        let n = season.season_number;
        let is_open = move || open_seasons.with(|o| o.as_ref().map_or(default_open, |v| v.contains(&n)));
        let toggle_open = move |_| {
            open_seasons.update(|o| {
                let mut v = o
                    .clone()
                    .unwrap_or_else(|| if default_open { vec![n] } else { Vec::new() });
                if let Some(p) = v.iter().position(|x| *x == n) {
                    v.remove(p);
                } else {
                    v.push(n);
                }
                *o = Some(v);
            });
        };
        let label = if n == 0 {
            "Specials".to_string()
        } else {
            format!("Season {n}")
        };
        let st = season.statistics.clone();
        let monitored = season.monitored;
        let label2 = label.clone();
        let label3 = label.clone();
        let label4 = label.clone();
        let label5 = label.clone();
        let eps = StoredValue::new(eps);
        view! {
            <section class="season" class:open=is_open>
                <div class="season-head">
                    <MonitorToggle monitored=monitored label=label.clone() on_toggle=Callback::new(move |()| toggle_season(n, monitored)) />
                    <button type="button" class="season-toggle" aria-expanded=move || is_open().to_string() on:click=toggle_open>
                        <span class="chev" aria-hidden="true">"▶"</span>
                        {label.clone()}
                    </button>
                    <EpisodeProgress have=st.episode_file_count total=st.episode_count />
                    <span class="muted">{format!("{} episodes", st.total_episode_count)}</span>
                    {(st.size_on_disk > 0).then(|| view! { <span class="muted">{bytes(st.size_on_disk)}</span> })}
                    <div class="season-actions">
                        <button type="button" class="icon-btn icon-btn-sm" title=format!("Search {label2}") aria-label=format!("Search {label2}")
                            on:click=move |_| start_search(LibrarySearchKind::Season, vec![id], Some(n), None, label3.clone())>
                            <Icon name="search" />
                        </button>
                        <button type="button" class="icon-btn icon-btn-sm" title=format!("Interactive search {label4}")
                            aria-label=format!("Interactive search {label4}")
                            on:click=move |_| interactive(SearchTarget::season(id, n, format!("{} · {label5}", series_title())))>
                            <Icon name="user" />
                        </button>
                    </div>
                </div>
                <Show when=is_open>
                    <div class="table-wrap">
                        <table class="table episode-table">
                            <caption class="visually-hidden">{format!("Episodes of {}", label.clone())}</caption>
                            <thead>
                                <tr>
                                    <th scope="col"><span class="visually-hidden">"Monitored"</span></th>
                                    <th scope="col">"#"</th>
                                    <th scope="col">"Title"</th>
                                    <th scope="col">"Air date"</th>
                                    <th scope="col">"Status"</th>
                                    <th scope="col">"Size"</th>
                                    <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                                </tr>
                            </thead>
                            <tbody>
                                {eps.get_value().into_iter().map(|e| {
                                    let eid = e.id;
                                    let (tone, status) = episode_status(&e);
                                    let code = episode_code(e.season_number, e.episode_number);
                                    let code2 = code.clone();
                                    let code3 = code.clone();
                                    let size = e.episode_file.as_ref().map(|f| bytes(f.file.size)).unwrap_or_default();
                                    view! {
                                        <tr>
                                            <td>
                                                <MonitorToggle monitored=e.monitored label=code.clone()
                                                    on_toggle=Callback::new(move |()| toggle_episodes(vec![eid], !e.monitored)) />
                                            </td>
                                            <td class="mono">
                                                {e.episode_number}
                                                {e.absolute_number.map(|a| view! { <span class="cell-sub">{format!("abs {a}")}</span> })}
                                            </td>
                                            <td>
                                                <span class="cell-strong">{e.title.clone().unwrap_or_else(|| "TBA".into())}</span>
                                                {e.finale.clone().map(|f| view! { <span class="badge tone-accent">{format!("{} finale", f)}</span> })}
                                            </td>
                                            <td>{e.air_date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "—".into())}</td>
                                            <td><StatusBadge tone=tone label=status /></td>
                                            <td>{size}</td>
                                            <td class="cell-actions">
                                                <button type="button" class="icon-btn icon-btn-sm" title=format!("Search {code}") aria-label=format!("Search {code}")
                                                    on:click=move |_| start_search(LibrarySearchKind::Episode, vec![eid], None, None, code2.clone())>
                                                    <Icon name="search" />
                                                </button>
                                                <button type="button" class="icon-btn icon-btn-sm" title=format!("Interactive search {code3}")
                                                    aria-label=format!("Interactive search {code3}")
                                                    on:click=move |_| interactive(SearchTarget::episode(eid, format!("{} · {}", series_title(), code3.clone())))>
                                                    <Icon name="user" />
                                                </button>
                                            </td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                </Show>
            </section>
        }
    };

    let seasons_view = move || {
        let (Some(Ok(s)), Some(eps)) = (series.get(), episodes.get()) else {
            return view! { <Skeleton lines=6 /> }.into_any();
        };
        let eps = match eps {
            Ok(e) => e,
            Err(e) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| episodes.refetch()) /> }
                    .into_any();
            }
        };
        if s.seasons.is_empty() {
            return view! { <EmptyState icon="series" title="No seasons yet" description="Refresh the series to load its episodes." compact=true /> }.into_any();
        }
        let mut seasons = s.seasons.clone();
        // Newest first; specials last.
        seasons.sort_by_key(|x| {
            if x.season_number == 0 {
                0
            } else {
                u32::MAX - x.season_number
            }
        });
        seasons.sort_by_key(|x| x.season_number == 0);
        let newest = seasons
            .iter()
            .find(|x| x.season_number != 0)
            .map(|x| x.season_number);
        seasons
            .into_iter()
            .map(|season| {
                let mut list: Vec<EpisodeDto> = eps
                    .iter()
                    .filter(|e| e.season_number == season.season_number)
                    .cloned()
                    .collect();
                list.sort_by_key(|e| std::cmp::Reverse(e.episode_number));
                let default_open = Some(season.season_number) == newest;
                season_block(season, list, default_open)
            })
            .collect_view()
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
        "files" => match files.get() {
            None => view! { <Skeleton lines=3 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
            Some(Ok(list)) if list.is_empty() => view! {
                <EmptyState icon="download" title="No files yet" description="Imported or scanned episode files show up here." compact=true />
            }
            .into_any(),
            Some(Ok(mut list)) => {
                list.sort_by(|a, b| (a.season_number, &a.file.relative_path).cmp(&(b.season_number, &b.file.relative_path)));
                let eps = episodes.get().and_then(Result::ok).unwrap_or_default();
                let rows = list
                    .into_iter()
                    .map(|f| {
                        let codes: Vec<String> = eps
                            .iter()
                            .filter(|e| f.episode_ids.contains(&e.id))
                            .map(|e| episode_code(e.season_number, e.episode_number))
                            .collect();
                        let label = if codes.is_empty() { format!("Season {}", f.season_number) } else { codes.join(" ") };
                        (Some(label), f.file)
                    })
                    .collect();
                view! {
                    <MediaFilesTable files=rows
                        on_delete=Callback::new(move |fid: i64| { file_delete.set(Some(fid)); file_delete_open.set(true); }) />
                }
                .into_any()
            }
        },
        "cast" => match series.get() {
            Some(Ok(s)) if !s.cast.is_empty() => view! { <CastRow cast=s.cast /> }.into_any(),
            Some(Ok(_)) => view! { <EmptyState icon="user" title="No cast information" compact=true /> }.into_any(),
            _ => view! { <Skeleton lines=2 /> }.into_any(),
        },
        "history" => view! { <HistoryTable series_id=id /> }.into_any(),
        _ => seasons_view().into_any(),
    }
    };

    view! {
        {header}
        <div class="tabs" role="tablist">
            {tab_btn("seasons", "Seasons")}
            {tab_btn("files", "Files")}
            {tab_btn("cast", "Cast")}
            {tab_btn("history", "History")}
        </div>
        <div class="series-tab">{tab_body}</div>
        <EditMediaModal kind=MediaKind::Series id=id open=edit_open fields=fields original_path=original_path opts=opts
            on_saved=Callback::new(move |()| series.refetch()) />
        <RenameModal open=rename_open kind=MediaKind::Series media_id=id />
        <InteractiveSearchModal open=isearch_open target=isearch_target />
        <ManualImportModal open=mi_open source=mi_source />
        <DeleteMediaDialog open=delete_open title="Delete series?"
            message=Signal::derive(move || series.get().and_then(Result::ok).map(|s| format!("“{}” will be removed from the library.", s.title)).unwrap_or_default())
            on_confirm=delete />
        <ConfirmDialog open=file_delete_open title="Delete file?"
            message=Signal::derive(|| "The file is removed from disk (or moved to the recycle bin).".to_string())
            confirm_label="Delete file" on_confirm=delete_file />
        <Modal open=pass_open title="Season pass">
            <div class="form">
                <p class="modal-text">"Choose which episodes of this series to monitor. Seasons and episodes are updated to match."</p>
                <div class="field">
                    <label for="sd-pass">"Monitor"</label>
                    <EnumSelect table=&SERIES_MONITOR value=pass_monitor id="sd-pass" />
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| pass_open.set(false)>"Cancel"</button>
                    <button type="button" class="btn btn-primary" on:click=season_pass>"Apply"</button>
                </div>
            </div>
        </Modal>
    }
}
