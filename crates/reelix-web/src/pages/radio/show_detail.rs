//! `/radio/shows/{id}` — one show: artwork, station, follow choice, monitoring, tags, root
//! folder and actions, then its episodes by series with an episode side panel (matched file,
//! "Not this file", "Match automatically again" and a manual choice from unmatched files).

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use reelix_api_types::EnqueuedJob;
use reelix_api_types::radio::{
    RadioEpisodeDto, RadioEpisodeFileDto, RadioEpisodeFileRequest, RadioFollow, RadioLibraryReviewDto,
    RadioSeriesKind, RadioShowDto, RadioShowEdit, RadioShowEpisodesDto,
};

use crate::api;
use crate::components::library_common::{LibIcon, bytes, on_entity_changed};
use crate::components::library_forms::{opt_from, require_library};
use crate::components::radio::{
    FOLLOW, RADIO_ENTITIES, RadioArtwork, RadioOptions, RadioProfileSelect, RadioRootSelect, RadioTagPicker,
    StationLogo, artwork_url, episode_code, file_label, length, opt_id, series_label, status_badge,
};
use crate::components::request_drawer::Drawer;
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, Modal, Skeleton, Spinner, StatusBadge};
use crate::format::http_url;
use crate::state::Toasts;

#[component]
pub fn RadioShowPage() -> impl IntoView {
    require_library();
    let params = use_params_map();
    let id = move || params.with(|p| p.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0));
    view! { {move || view! { <ShowDetail id=id() /> }} }
}

/// Saves a partial edit of the show and reloads it.
fn save_edit(id: i64, edit: RadioShowEdit, ok: &'static str, done: impl Fn() + 'static) {
    let toasts = Toasts::expect();
    spawn_local(async move {
        match api::put::<_, RadioShowDto>(&format!("/radio/shows/{id}"), &edit).await {
            Ok(_) => {
                toasts.success(ok);
                done();
            }
            Err(e) => toasts.error(format!("Could not save: {e}")),
        }
    });
}

#[component]
fn ShowDetail(id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let opts = RadioOptions::load();
    let show =
        LocalResource::new(
            move || async move { api::get::<RadioShowDto>(&format!("/radio/shows/{id}")).await },
        );
    let episodes = LocalResource::new(move || async move {
        api::get::<RadioShowEpisodesDto>(&format!("/radio/shows/{id}/episodes")).await
    });
    let selected = RwSignal::new(None::<i64>);
    let drawer_open = RwSignal::new(false);
    let episode_rev = RwSignal::new(0u32);
    on_entity_changed(RADIO_ENTITIES, true, move || {
        show.refetch();
        episodes.refetch();
        episode_rev.update(|n| *n += 1);
    });

    let edit_open = RwSignal::new(false);
    let delete_open = RwSignal::new(false);
    let reload = move || {
        show.refetch();
        episodes.refetch();
    };

    let refresh = move |_| {
        spawn_local(async move {
            match api::post_empty::<EnqueuedJob>(&format!("/radio/shows/{id}/refresh")).await {
                Ok(job) if job.job_id.is_none() => toasts.info("A refresh of this show is already queued."),
                Ok(_) => toasts.success("Refresh queued."),
                Err(e) => toasts.error(format!("Could not start the refresh: {e}")),
            }
        });
    };

    let delete = Callback::new(move |()| {
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::delete(&format!("/radio/shows/{id}")).await {
                Ok(()) => {
                    toasts.success("Show deleted. Files in the podcast library were kept.");
                    navigate("/radio", Default::default());
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    let header = move || {
        let s = match show.get() {
            None => return view! { <div class="detail-hero"><Skeleton lines=5 /></div> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| show.refetch()) /> }
                    .into_any();
            }
            Some(Ok(s)) => s,
        };
        document().set_title(&format!("{} · Reelix", s.title));
        let st = s.statistics.clone();
        let tags = opts.tag_names(&s.tags);
        let (root, profile) = (s.root_folder_id, s.quality_profile_id);
        let monitored = s.monitored;
        let follow = s.follow;
        let web = s.web_url.as_deref().and_then(http_url);
        let feed = s.feed_url.as_deref().and_then(http_url);
        let station = s.station_id.zip(s.station_name.clone());
        view! {
            <div class="detail-hero">
                <div class="detail-inner">
                    <RadioArtwork title=s.title.clone() url=artwork_url(&s) />
                    <div class="detail-info">
                        <h1 class="detail-title">{s.title.clone()}</h1>
                        <div class="meta-line">
                            {station.map(|(sid, name)| view! {
                                <A href=format!("/radio?station={sid}") attr:class="station-link">
                                    <StationLogo station_id=sid name=name.clone() />
                                    <span>{name.clone()}</span>
                                </A>
                            })}
                            {s.author.clone().map(|a| view! { <span>{a}</span> })}
                            {s.first_broadcast.map(|d| view! { <span>{format!("First broadcast {}", crate::format::date(d))}</span> })}
                            <span>{format!("{} series", st.series_count)}</span>
                            <span>{format!("{} / {} episodes in library", st.in_library_count, st.episode_count)}</span>
                            {(st.wanted_count > 0).then(|| view! {
                                <StatusBadge tone=crate::components::Tone::Danger label=format!("{} wanted", st.wanted_count) dot=true />
                            })}
                        </div>
                        {s.synopsis.clone().map(|o| view! { <p class="detail-overview">{o}</p> })}
                        <div class="detail-facts">
                            <div class="fact">
                                <label class="fact-label" for="radio-follow">"Follow"</label>
                                <select id="radio-follow" class="input input-sm" on:change=move |e| {
                                    let Some(f) = opt_from(&FOLLOW, &event_target_value(&e)) else { return };
                                    if f == follow { return; }
                                    let edit = RadioShowEdit { follow: Some(f), ..Default::default() };
                                    save_edit(id, edit, "Follow choice saved; episode monitoring updated.", reload);
                                }>
                                    {FOLLOW.iter().map(|(v, k, l)| {
                                        let v = *v;
                                        view! { <option value=*k selected=v == follow>{*l}</option> }
                                    }).collect_view()}
                                </select>
                            </div>
                            <div class="fact"><span class="fact-label">"Monitored"</span>
                                <span>{if monitored { "Yes" } else { "No" }}</span>
                            </div>
                            {s.followed_at.filter(|_| follow == RadioFollow::FromNowOn).map(|d| view! {
                                <div class="fact"><span class="fact-label">"Followed since"</span><span>{crate::format::date(d)}</span></div>
                            })}
                            <div class="fact"><span class="fact-label">"Root folder"</span>
                                <span class="mono">{move || opts.root_path(root)}</span>
                            </div>
                            <div class="fact"><span class="fact-label">"Quality profile"</span>
                                <span>{move || opts.profile_name(profile)}</span>
                            </div>
                            <div class="fact"><span class="fact-label">"Source"</span><span>{s.provider.clone()}</span></div>
                            {s.refreshed_at.map(|d| view! {
                                <div class="fact"><span class="fact-label">"Refreshed"</span><span>{crate::format::relative(d)}</span></div>
                            })}
                            {(!tags.is_empty()).then(|| view! {
                                <div class="fact"><span class="fact-label">"Tags"</span>
                                    <span class="chip-list">{tags.into_iter().map(|t| view! { <span class="chip">{t}</span> }).collect_view()}</span>
                                </div>
                            })}
                            {s.abs_item_id.clone().map(|item| view! {
                                // TODO: link to the item in Audiobookshelf once the API exposes its base URL.
                                <div class="fact"><span class="fact-label">"Audiobookshelf item"</span><span class="mono">{item}</span></div>
                            })}
                        </div>
                        <div class="detail-actions">
                            <button type="button" class="btn btn-primary" on:click=refresh>
                                <Icon name="refresh" />
                                "Refresh"
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| {
                                let edit = RadioShowEdit { monitored: Some(!monitored), ..Default::default() };
                                save_edit(id, edit, if monitored { "Show unmonitored." } else { "Show monitored." }, reload);
                            }>
                                <LibIcon name=if monitored { "bookmark-off" } else { "bookmark" } />
                                {if monitored { "Unmonitor" } else { "Monitor" }}
                            </button>
                            <button type="button" class="btn btn-secondary" on:click=move |_| edit_open.set(true)>
                                <LibIcon name="edit" />
                                "Edit"
                            </button>
                            {web.map(|u| view! {
                                <a class="btn btn-ghost" href=u target="_blank" rel="noopener noreferrer">
                                    <Icon name="external" />
                                    "Show page"
                                </a>
                            })}
                            {feed.map(|u| view! {
                                <a class="btn btn-ghost" href=u target="_blank" rel="noopener noreferrer">
                                    <Icon name="external" />
                                    "Feed"
                                </a>
                            })}
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

    let open_episode = Callback::new(move |eid: i64| {
        selected.set(Some(eid));
        drawer_open.set(true);
    });

    let show_sig = Signal::derive(move || show.get().and_then(Result::ok));

    view! {
        {header}
        <EpisodeList episodes=episodes on_open=open_episode />
        <ShowEditModal open=edit_open show=show_sig opts=opts on_saved=Callback::new(move |()| reload()) />
        <ConfirmDialog open=delete_open title="Delete show?"
            message=Signal::derive(move || show_sig.get().map(|s| format!("“{}” and its series and episodes will be removed from Reelix. Files in the podcast library are kept.", s.title)).unwrap_or_default())
            confirm_label="Delete" on_confirm=delete />
        <EpisodePanel open=drawer_open episode_id=selected show=show_sig rev=episode_rev
            on_changed=Callback::new(move |()| reload()) />
    }
}

/// Series picker and episode table.
#[component]
fn EpisodeList(
    episodes: LocalResource<api::ApiResult<RadioShowEpisodesDto>>,
    on_open: Callback<i64>,
) -> impl IntoView {
    let series = RwSignal::new(0i64);
    // Default to the newest numbered series (specials last).
    Effect::new(move |_| {
        if let Some(Ok(d)) = episodes.get()
            && !d.series.iter().any(|s| s.id == series.get_untracked())
        {
            let pick = d
                .series
                .iter()
                .filter(|s| s.kind == RadioSeriesKind::Series)
                .max_by_key(|s| s.number.unwrap_or(0))
                .or_else(|| d.series.first())
                .map_or(0, |s| s.id);
            series.set(pick);
        }
    });

    move || {
        let d = match episodes.get() {
            None => return view! { <Skeleton lines=4 /> }.into_any(),
            Some(Err(e)) => {
                return view! { <ErrorBox error=e on_retry=Callback::new(move |()| episodes.refetch()) /> }
                    .into_any();
            }
            Some(Ok(d)) => d,
        };
        if d.series.is_empty() {
            return view! {
                <div class="card">
                    <EmptyState icon="radio" title="No episodes yet" description="Refresh the show to read its series and episodes from the source." />
                </div>
            }
            .into_any();
        }
        let mut list = d.series.clone();
        list.sort_by_key(|s| {
            (
                s.kind == RadioSeriesKind::Specials,
                std::cmp::Reverse(s.number.unwrap_or(0)),
            )
        });
        let current = series.get();
        let cur = list.iter().find(|s| s.id == current).cloned();
        let rows: Vec<RadioEpisodeDto> = d
            .episodes
            .into_iter()
            .filter(|e| e.series_id == current)
            .collect();
        let n = rows.len();
        let subtitle = cur
            .as_ref()
            .map(|s| {
                let mut t = format!("{n} episode(s)");
                if let Some(x) = s.expected_episodes {
                    t.push_str(&format!(" of {x} expected"));
                }
                if s.settled_at.is_some() {
                    t.push_str(" · complete");
                }
                if s.source_removed_at.is_some() {
                    t.push_str(" · no longer listed by the source");
                }
                t
            })
            .unwrap_or_default();
        view! {
            <section class="card">
                <header class="card-header">
                    <div>
                        <h2 class="card-title">"Episodes"</h2>
                        <p class="card-subtitle">{subtitle}</p>
                    </div>
                    <div class="card-actions">
                        <label class="visually-hidden" for="radio-series">"Series"</label>
                        <select id="radio-series" class="input" on:change=move |e| series.set(event_target_value(&e).parse().unwrap_or(0))>
                            {list.iter().map(|s| {
                                let sid = s.id;
                                view! { <option value=sid.to_string() selected=sid == current>{format!("{} ({})", series_label(s), s.episode_count)}</option> }
                            }).collect_view()}
                        </select>
                    </div>
                </header>
                <div class="card-body">
                    {cur.as_ref().and_then(|s| s.synopsis.clone()).map(|t| view! { <p class="muted">{t}</p> })}
                    {if rows.is_empty() {
                        view! { <EmptyState icon="radio" title="No episodes in this series" compact=true /> }.into_any()
                    } else {
                        view! {
                            <div class="table-wrap">
                                <table class="table radio-episodes">
                                    <caption class="visually-hidden">"Episodes"</caption>
                                    <thead>
                                        <tr>
                                            <th scope="col"><span class="visually-hidden">"Monitored"</span></th>
                                            <th scope="col">"#"</th>
                                            <th scope="col">"Title"</th>
                                            <th scope="col">"First broadcast"</th>
                                            <th scope="col">"Length"</th>
                                            <th scope="col">"Status"</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {rows.into_iter().map(|e| {
                                            let eid = e.id;
                                            let (tone, label) = status_badge(e.status);
                                            let mon = e.monitored;
                                            view! {
                                                <tr class:muted=e.source_removed_at.is_some()>
                                                    <td>
                                                        <span class="monitor-flag" title=if mon { "Monitored" } else { "Not monitored" }>
                                                            <LibIcon name=if mon { "bookmark" } else { "bookmark-off" } />
                                                            <span class="visually-hidden">{if mon { "Monitored" } else { "Not monitored" }}</span>
                                                        </span>
                                                    </td>
                                                    <td class="mono nowrap">{episode_code(&e)}</td>
                                                    <td class="cell-strong">
                                                        <button type="button" class="link-btn" on:click=move |_| on_open.run(eid)>{e.title.clone()}</button>
                                                        {e.source_removed_at.map(|_| view! { <span class="cell-sub">"No longer listed by the source"</span> })}
                                                    </td>
                                                    <td class="nowrap">{e.first_broadcast.map(crate::format::date).unwrap_or_else(|| "—".into())}</td>
                                                    <td class="nowrap">{length(e.duration_secs)}</td>
                                                    <td><StatusBadge tone=tone label=label /></td>
                                                </tr>
                                            }
                                        }).collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        }.into_any()
                    }}
                </div>
            </section>
        }
        .into_any()
    }
}

/// Edit tags, root folder and quality profile.
#[component]
fn ShowEditModal(
    open: RwSignal<bool>,
    #[prop(into)] show: Signal<Option<RadioShowDto>>,
    opts: RadioOptions,
    on_saved: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let follow = RwSignal::new(RadioFollow::All);
    let monitored = RwSignal::new(true);
    let tags = RwSignal::new(Vec::<i64>::new());
    let root = RwSignal::new(0i64);
    let profile = RwSignal::new(0i64);
    let busy = RwSignal::new(false);
    Effect::new(move |_| {
        if open.get()
            && let Some(s) = show.get_untracked()
        {
            follow.set(s.follow);
            monitored.set(s.monitored);
            tags.set(s.tags.clone());
            root.set(s.root_folder_id.unwrap_or(0));
            profile.set(s.quality_profile_id.unwrap_or(0));
        }
    });
    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(s) = show.get_untracked() else { return };
        let edit = RadioShowEdit {
            follow: Some(follow.get_untracked()).filter(|f| *f != s.follow),
            monitored: Some(monitored.get_untracked()),
            tags: Some(tags.get_untracked()),
            root_folder_id: Some(opt_id(root.get_untracked())),
            quality_profile_id: Some(opt_id(profile.get_untracked())),
            abs_item_id: None,
        };
        busy.set(true);
        spawn_local(async move {
            let res = api::put::<_, RadioShowDto>(&format!("/radio/shows/{}", s.id), &edit).await;
            busy.set(false);
            match res {
                Ok(_) => {
                    toasts.success("Show saved.");
                    open.set(false);
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };
    view! {
        <Modal open=open title="Edit show">
            <form class="form" on:submit=submit>
                <div class="field">
                    <label for="rse-follow">"Follow"</label>
                    <select id="rse-follow" class="input" on:change=move |e| {
                        if let Some(f) = opt_from(&FOLLOW, &event_target_value(&e)) { follow.set(f); }
                    }>
                        {FOLLOW.iter().map(|(v, k, l)| {
                            let v = *v;
                            view! { <option value=*k selected=move || follow.get() == v>{*l}</option> }
                        }).collect_view()}
                    </select>
                    <span class="field-hint">"Changing it re-applies the choice to every episode; “from now on” starts counting today."</span>
                </div>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || monitored.get() on:change=move |e| monitored.set(event_target_checked(&e)) />
                    "Monitored"
                </label>
                <div class="field">
                    <label for="rse-root">"Root folder"</label>
                    <RadioRootSelect opts=opts value=root id="rse-root" />
                </div>
                <div class="field">
                    <label for="rse-profile">"Quality profile"</label>
                    <RadioProfileSelect opts=opts value=profile id="rse-profile" />
                    <span class="field-hint">"An audiobook profile, used when episodes are searched for on indexers."</span>
                </div>
                <div class="field">
                    <span class="field-label">"Tags"</span>
                    <RadioTagPicker opts=opts value=tags />
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {move || if busy.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="check" /> }.into_any() }}
                        "Save"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Unmatched files, the ones that look like this show's first.
fn ranked_files(
    mut files: Vec<RadioEpisodeFileDto>,
    show: Option<&RadioShowDto>,
) -> Vec<RadioEpisodeFileDto> {
    let (item, title) = show.map_or((None, String::new()), |s| {
        (s.abs_item_id.clone(), s.title.to_lowercase())
    });
    files.sort_by_key(|f| {
        let same_item = item.as_deref() == Some(f.abs_item_id.as_str());
        let same_title = f
            .item_title
            .as_deref()
            .is_some_and(|t| !title.is_empty() && t.to_lowercase() == title);
        (!(same_item || same_title), f.item_title.clone(), f.published_at)
    });
    files
}

/// Right-hand panel of one episode.
#[component]
fn EpisodePanel(
    open: RwSignal<bool>,
    episode_id: RwSignal<Option<i64>>,
    #[prop(into)] show: Signal<Option<RadioShowDto>>,
    rev: RwSignal<u32>,
    on_changed: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let local_rev = RwSignal::new(0u32);
    let episode = LocalResource::new(move || {
        let id = episode_id.get();
        let _ = (rev.get(), local_rev.get());
        async move {
            match id {
                Some(id) => Some(api::get::<RadioEpisodeDto>(&format!("/radio/episodes/{id}")).await),
                None => None,
            }
        }
    });
    let review = LocalResource::new(move || {
        let want = open.get();
        let _ = (rev.get(), local_rev.get());
        async move {
            if want {
                Some(api::get::<RadioLibraryReviewDto>("/radio/library/review").await)
            } else {
                None
            }
        }
    });
    let choice = RwSignal::new(0i64);
    let filter = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    Effect::new(move |_| {
        let _ = episode_id.get();
        choice.set(0);
        filter.set(String::new());
    });

    let title = Signal::derive(move || {
        episode
            .get()
            .flatten()
            .and_then(Result::ok)
            .map(|e| format!("{} · {}", episode_code(&e), e.title))
            .unwrap_or_else(|| "Episode".into())
    });

    let changed = move |msg: &'static str| {
        toasts.success(msg);
        local_rev.update(|n| *n += 1);
        on_changed.run(());
    };

    let clear = move |id: i64, auto: bool| {
        busy.set(true);
        spawn_local(async move {
            let res = api::delete(&format!("/radio/episodes/{id}/file?auto={auto}")).await;
            busy.set(false);
            match res {
                Ok(()) => changed(if auto {
                    "The next library sync may match this episode again."
                } else {
                    "File unlinked; the episode stays unmatched until you choose a file."
                }),
                Err(e) => toasts.error(format!("Could not change the match: {e}")),
            }
        });
    };

    let set_file = move |id: i64| {
        let file_id = choice.get_untracked();
        if file_id == 0 {
            toasts.error("Choose a file first.");
            return;
        }
        busy.set(true);
        spawn_local(async move {
            let res = api::put::<_, RadioEpisodeDto>(
                &format!("/radio/episodes/{id}/file"),
                &RadioEpisodeFileRequest { file_id },
            )
            .await;
            busy.set(false);
            match res {
                Ok(_) => changed("File matched by hand."),
                Err(e) => toasts.error(format!("Could not match the file: {e}")),
            }
        });
    };

    let body = move || {
        let e = match episode.get() {
            None | Some(None) => return view! { <Skeleton lines=5 /> }.into_any(),
            Some(Some(Err(err))) => return view! { <ErrorBox error=err /> }.into_any(),
            Some(Some(Ok(e))) => e,
        };
        let eid = e.id;
        let (tone, label) = status_badge(e.status);
        let enclosure = e.enclosure_url.as_deref().and_then(http_url);
        let file = e.file.clone();
        let cleared_by_hand = e.manual_match && e.file_id.is_none();
        let file_view = match file {
            Some(f) => view! {
                <section class="stack">
                    <h3 class="form-section">"Matched file"</h3>
                    <dl class="kv">
                        {f.item_title.clone().map(|t| view! { <div class="kv-row"><dt>"Podcast"</dt><dd>{t}</dd></div> })}
                        {f.title.clone().map(|t| view! { <div class="kv-row"><dt>"Title"</dt><dd>{t}</dd></div> })}
                        {(f.season.is_some() || f.episode.is_some()).then(|| view! {
                            <div class="kv-row"><dt>"Number"</dt><dd>{format!(
                                "{}{}",
                                f.season.map(|s| format!("S{s:02}")).unwrap_or_default(),
                                f.episode.map(|n| format!("E{n:02}")).unwrap_or_default()
                            )}</dd></div>
                        })}
                        {f.published_at.map(|d| view! { <div class="kv-row"><dt>"Published"</dt><dd>{crate::format::date(d)}</dd></div> })}
                        <div class="kv-row"><dt>"Length"</dt><dd>{length(f.duration_secs)}</dd></div>
                        {f.size.map(|s| view! { <div class="kv-row"><dt>"Size"</dt><dd>{bytes(s)}</dd></div> })}
                        {f.path.clone().map(|p| view! { <div class="kv-row"><dt>"Path"</dt><dd class="mono">{p}</dd></div> })}
                        <div class="kv-row"><dt>"Matched"</dt><dd>{if e.manual_match { "By hand" } else { "Automatically" }}</dd></div>
                    </dl>
                    <div class="drawer-actions">
                        <button type="button" class="btn btn-secondary" disabled=move || busy.get() on:click=move |_| clear(eid, false)>
                            <Icon name="close" />
                            "Not this file"
                        </button>
                        <button type="button" class="btn btn-ghost" disabled=move || busy.get() on:click=move |_| clear(eid, true)>
                            <Icon name="refresh" />
                            "Match automatically again"
                        </button>
                    </div>
                </section>
            }
            .into_any(),
            None => view! {
                <section class="stack">
                    <h3 class="form-section">"Matched file"</h3>
                    <p class="muted">{if cleared_by_hand {
                        "No file. A file was unlinked by hand, so automatic matching leaves this episode alone."
                    } else {
                        "No file in the podcast library is matched to this episode yet."
                    }}</p>
                    {cleared_by_hand.then(|| view! {
                        <div class="drawer-actions">
                            <button type="button" class="btn btn-ghost" disabled=move || busy.get() on:click=move |_| clear(eid, true)>
                                <Icon name="refresh" />
                                "Match automatically again"
                            </button>
                        </div>
                    })}
                </section>
            }
            .into_any(),
        };

        let picker = move || {
            let files = match review.get() {
                None | Some(None) => return view! { <Skeleton lines=2 /> }.into_any(),
                Some(Some(Err(err))) => return view! { <ErrorBox error=err /> }.into_any(),
                Some(Some(Ok(r))) => r.unmatched_files,
            };
            if files.is_empty() {
                return view! { <p class="field-hint">"There are no unmatched files in the podcast library."</p> }.into_any();
            }
            let needle = filter.get().trim().to_lowercase();
            let files: Vec<RadioEpisodeFileDto> = ranked_files(files, show.get_untracked().as_ref())
                .into_iter()
                .filter(|f| needle.is_empty() || file_label(f).to_lowercase().contains(&needle))
                .collect();
            view! {
                <div class="field">
                    <label for="radio-file-filter">"Filter files"</label>
                    <input id="radio-file-filter" class="input" type="search" placeholder="Title, podcast or date…"
                        prop:value=move || filter.get() on:input=move |e| filter.set(event_target_value(&e)) />
                </div>
                <div class="field">
                    <label for="radio-file-choice">"File"</label>
                    <select id="radio-file-choice" class="input" on:change=move |e| choice.set(event_target_value(&e).parse().unwrap_or(0))>
                        <option value="0" selected=move || choice.get() == 0>{format!("Choose one of {} file(s)…", files.len())}</option>
                        {files.iter().map(|f| {
                            let fid = f.id;
                            view! { <option value=fid.to_string() selected=move || choice.get() == fid>{file_label(f)}</option> }
                        }).collect_view()}
                    </select>
                </div>
                <div class="drawer-actions">
                    <button type="button" class="btn btn-primary" disabled=move || busy.get() || choice.get() == 0
                        on:click=move |_| set_file(eid)>
                        <Icon name="check" />
                        "Use this file"
                    </button>
                </div>
            }
            .into_any()
        };

        view! {
            <div class="drawer-status">
                <StatusBadge tone=tone label=label dot=true />
                <span class="badge tone-neutral">{if e.monitored { "Monitored" } else { "Not monitored" }}</span>
                {e.source_removed_at.map(|_| view! { <span class="badge tone-warning">"No longer listed by the source"</span> })}
            </div>
            <dl class="kv">
                <div class="kv-row"><dt>"Number"</dt><dd class="mono">{episode_code(&e)}</dd></div>
                <div class="kv-row"><dt>"First broadcast"</dt><dd>{e.first_broadcast.map(crate::format::datetime).unwrap_or_else(|| "—".into())}</dd></div>
                <div class="kv-row"><dt>"Length"</dt><dd>{length(e.duration_secs)}</dd></div>
                {show.get().map(|s| view! { <div class="kv-row"><dt>"Show"</dt><dd>{s.title}</dd></div> })}
                {enclosure.map(|u| view! {
                    <div class="kv-row"><dt>"Audio"</dt><dd><a href=u target="_blank" rel="noopener noreferrer">"Source audio"<Icon name="external" /></a></dd></div>
                })}
            </dl>
            {e.synopsis.clone().map(|t| view! { <p class="detail-overview">{t}</p> })}
            {file_view}
            <section class="stack">
                <h3 class="form-section">{if e.file_id.is_some() { "Choose another file" } else { "Choose a file" }}</h3>
                <p class="field-hint">"Files in the podcast library that no episode is matched to. A file chosen by hand is kept by automatic matching."</p>
                {picker}
            </section>
        }
        .into_any()
    };

    view! {
        <Drawer open=open title=title>
            {body}
        </Drawer>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::library_forms::opt_key;
    use crate::components::radio::follow_label;

    #[test]
    fn follow_keys_round_trip() {
        for (v, k, _) in FOLLOW {
            assert_eq!(opt_from(&FOLLOW, k), Some(v));
            assert_eq!(opt_key(&FOLLOW, v), k);
        }
        assert_eq!(follow_label(RadioFollow::FromNowOn), "From now on");
    }
}
