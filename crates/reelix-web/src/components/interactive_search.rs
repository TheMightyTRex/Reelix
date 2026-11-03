//! Interactive search modal: search every indexer for one movie / season / episode / book format
//! / album, show the decision for each release (rejection reasons, custom-format score, rank) and
//! grab one.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::pipeline::{GrabRequest, GrabResponse, ReleaseDto, ReleaseQuery};
use reelix_core::{BookFormat, Protocol};

use super::history_table::{FormatScore, ProtoBadge, episodes_label, languages_label, quality_label};
use super::library_common::{SortHeader, bytes};
use super::library_forms::with_query;
use super::{EmptyState, ErrorBox, Icon, Modal, Spinner};
use crate::api::{self, ApiError};
use crate::state::Toasts;

/// What to search for and the title shown in the modal header.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchTarget {
    pub title: String,
    pub query: ReleaseQuery,
}

impl SearchTarget {
    pub fn movie(id: i64, title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            query: ReleaseQuery {
                movie_id: Some(id),
                ..Default::default()
            },
        }
    }

    pub fn season(series_id: i64, season: u32, title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            query: ReleaseQuery {
                series_id: Some(series_id),
                season: Some(season),
                ..Default::default()
            },
        }
    }

    pub fn episode(episode_id: i64, title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            query: ReleaseQuery {
                episode_id: Some(episode_id),
                ..Default::default()
            },
        }
    }

    /// One format of a book (`None` = the first format the author has a profile for).
    pub fn book(book_id: i64, format: Option<BookFormat>, title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            query: ReleaseQuery {
                book_id: Some(book_id),
                book_format: format,
                ..Default::default()
            },
        }
    }

    pub fn album(album_id: i64, title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            query: ReleaseQuery {
                album_id: Some(album_id),
                ..Default::default()
            },
        }
    }

    fn url(&self) -> String {
        let q = &self.query;
        let s = |v: Option<i64>| v.map(|v| v.to_string()).unwrap_or_default();
        with_query(
            "/releases",
            &[
                ("movie_id", s(q.movie_id)),
                ("series_id", s(q.series_id)),
                ("season", q.season.map(|v| v.to_string()).unwrap_or_default()),
                ("episode_id", s(q.episode_id)),
                ("book_id", s(q.book_id)),
                (
                    "book_format",
                    q.book_format.map(|f| f.as_str().to_string()).unwrap_or_default(),
                ),
                ("album_id", s(q.album_id)),
            ],
        )
    }
}

/// `45m`, `5h`, `3d`.
pub fn age_label(minutes: Option<i64>) -> String {
    match minutes {
        None => "—".into(),
        Some(m) if m < 60 => format!("{}m", m.max(0)),
        Some(m) if m < 48 * 60 => format!("{}h", m / 60),
        Some(m) => format!("{}d", m / 1440),
    }
}

/// Sorts releases by a column key (`rank` default, descending = best first).
fn sort_releases(list: &mut [ReleaseDto], key: &str, desc: bool) {
    match key {
        "age" => list.sort_by_key(|r| r.age_minutes.unwrap_or(i64::MAX)),
        "title" => list.sort_by_key(|r| r.title.to_lowercase()),
        "indexer" => list.sort_by_key(|r| r.indexer.to_lowercase()),
        "size" => list.sort_by_key(|r| r.size.unwrap_or(0)),
        "peers" => list.sort_by_key(|r| r.seeders.unwrap_or(0)),
        "quality" => list.sort_by_key(|r| (r.quality.quality.id(), r.quality.revision)),
        "score" => list.sort_by_key(|r| r.format_score),
        "protocol" => list.sort_by_key(|r| r.protocol == Protocol::Usenet),
        // Rank: higher is better.
        _ => list.sort_by_key(|r| std::cmp::Reverse(r.rank)),
    }
    if desc {
        list.reverse();
    }
}

/// The interactive search dialog. Set `target` and `open` to start a search.
#[component]
pub fn InteractiveSearchModal(open: RwSignal<bool>, target: RwSignal<Option<SearchTarget>>) -> impl IntoView {
    let toasts = Toasts::expect();
    let loading = RwSignal::new(false);
    let results = RwSignal::new(None::<Result<Vec<ReleaseDto>, ApiError>>);
    // Bumped per search so a slow, superseded response is dropped.
    let generation = StoredValue::new(0u64);
    let quality_filter = RwSignal::new(String::new());
    let indexer_filter = RwSignal::new(String::new());
    let protocol_filter = RwSignal::new(String::new());
    let show_rejected = RwSignal::new(true);
    let sort = RwSignal::new("rank".to_string());
    let desc = RwSignal::new(false);
    let grabbing = RwSignal::new(None::<String>);
    let grabbed = RwSignal::new(Vec::<String>::new());

    let run = move || {
        let Some(t) = target.get_untracked() else {
            return;
        };
        let gen_id = generation.get_value() + 1;
        generation.set_value(gen_id);
        loading.set(true);
        results.set(None);
        grabbed.set(Vec::new());
        quality_filter.set(String::new());
        indexer_filter.set(String::new());
        protocol_filter.set(String::new());
        let url = t.url();
        spawn_local(async move {
            let r = api::get::<Vec<ReleaseDto>>(&url).await;
            if generation.get_value() == gen_id {
                let _ = results.try_set(Some(r));
                let _ = loading.try_set(false);
            }
        });
    };

    Effect::new(move |_| {
        if open.get() && target.with(Option::is_some) {
            run();
        }
    });

    let grab = Callback::new(move |(r, force): (ReleaseDto, bool)| {
        let key = format!("{}|{}", r.indexer_id, r.guid);
        grabbing.set(Some(key.clone()));
        let req = GrabRequest {
            guid: r.guid.clone(),
            indexer_id: r.indexer_id,
            force,
            movie_id: None,
            episode_ids: None,
            download_client_id: None,
            download_url: None,
            title: None,
        };
        spawn_local(async move {
            match api::post::<_, GrabResponse>("/releases/grab", &req).await {
                Ok(resp) => {
                    toasts.success(format!("Sent “{}” to {}.", r.title, resp.download_client));
                    grabbed.update(|g| g.push(key));
                }
                Err(e) => toasts.error(format!("Grab failed: {e}")),
            }
            grabbing.set(None);
        });
    });

    let list = move || {
        let mut rel = match results.get() {
            Some(Ok(r)) => r,
            _ => return Vec::new(),
        };
        let qf = quality_filter.get();
        let inf = indexer_filter.get();
        let pf = protocol_filter.get();
        let rejected_ok = show_rejected.get();
        rel.retain(|r| {
            (qf.is_empty() || r.quality.quality.name() == qf)
                && (inf.is_empty() || r.indexer == inf)
                && (pf.is_empty() || super::history_table::proto_key(r.protocol) == pf)
                && (rejected_ok || r.accepted)
        });
        sort_releases(&mut rel, &sort.get(), desc.get());
        rel
    };

    let options = move || {
        let all = match results.get() {
            Some(Ok(r)) => r,
            _ => Vec::new(),
        };
        let mut qualities: Vec<(u16, &'static str)> = all
            .iter()
            .map(|r| (r.quality.quality.id(), r.quality.quality.name()))
            .collect();
        qualities.sort_unstable();
        qualities.dedup();
        let mut indexers: Vec<String> = all.iter().map(|r| r.indexer.clone()).collect();
        indexers.sort_unstable();
        indexers.dedup();
        let accepted = all.iter().filter(|r| r.accepted).count();
        (qualities, indexers, all.len(), accepted)
    };

    let title = Signal::derive(move || target.get().map(|t| t.title).unwrap_or_default());

    let table = move || {
        if loading.get() {
            return view! {
                <div class="isearch-loading">
                    <Spinner label="Searching indexers…" />
                    <p class="muted">"Waiting for indexers to respond. Slow indexers can take up to a minute."</p>
                </div>
            }
            .into_any();
        }
        match results.get() {
            None => ().into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| run()) /> }.into_any(),
            Some(Ok(all)) if all.is_empty() => view! {
                <EmptyState icon="search" title="No releases found" compact=true
                    description="No indexer returned a result. Check that indexers with interactive search are enabled." />
            }
            .into_any(),
            Some(Ok(_)) => {
                let rows = list();
                if rows.is_empty() {
                    return view! { <EmptyState icon="search" title="Nothing matches the filters" compact=true /> }.into_any();
                }
                view! {
                    <div class="table-wrap isearch-table">
                        <table class="table">
                            <caption class="visually-hidden">"Releases"</caption>
                            <thead>
                                <tr>
                                    <SortHeader label="Proto" key="protocol" sort=sort desc=desc />
                                    <SortHeader label="Age" key="age" sort=sort desc=desc />
                                    <SortHeader label="Title" key="title" sort=sort desc=desc />
                                    <SortHeader label="Indexer" key="indexer" sort=sort desc=desc />
                                    <SortHeader label="Size" key="size" sort=sort desc=desc />
                                    <SortHeader label="Peers" key="peers" sort=sort desc=desc />
                                    <th scope="col">"Language"</th>
                                    <SortHeader label="Quality" key="quality" sort=sort desc=desc />
                                    <SortHeader label="CF" key="score" sort=sort desc=desc />
                                    <SortHeader label="Rank" key="rank" sort=sort desc=desc />
                                    <th scope="col"><span class="visually-hidden">"Grab"</span></th>
                                </tr>
                            </thead>
                            <tbody>
                                {rows.into_iter().map(|r| release_row(r, grabbing, grabbed, grab)).collect_view()}
                            </tbody>
                        </table>
                    </div>
                }
                .into_any()
            }
        }
    };

    view! {
        <Modal open=open title="Interactive search" on_close=Callback::new(move |()| {
            generation.update_value(|g| *g += 1);
            loading.set(false);
        })>
            <div class="isearch">
                <h3 class="isearch-target">{move || title.get()}</h3>
                <div class="toolbar isearch-filters">
                    {move || {
                        let (qualities, indexers, total, accepted) = options();
                        view! {
                            <label class="visually-hidden" for="isr-q">"Quality"</label>
                            <select id="isr-q" class="input input-sm" on:change=move |e| quality_filter.set(event_target_value(&e))
                                prop:value=move || quality_filter.get()>
                                <option value="">"All qualities"</option>
                                {qualities.into_iter().map(|(_, n)| view! { <option value=n>{n}</option> }).collect_view()}
                            </select>
                            <label class="visually-hidden" for="isr-i">"Indexer"</label>
                            <select id="isr-i" class="input input-sm" on:change=move |e| indexer_filter.set(event_target_value(&e))
                                prop:value=move || indexer_filter.get()>
                                <option value="">"All indexers"</option>
                                {indexers.into_iter().map(|n| view! { <option value=n.clone()>{n.clone()}</option> }).collect_view()}
                            </select>
                            <label class="visually-hidden" for="isr-p">"Protocol"</label>
                            <select id="isr-p" class="input input-sm" on:change=move |e| protocol_filter.set(event_target_value(&e))
                                prop:value=move || protocol_filter.get()>
                                <option value="">"Torrent + Usenet"</option>
                                <option value="torrent">"Torrent"</option>
                                <option value="usenet">"Usenet"</option>
                            </select>
                            <label class="checkbox">
                                <input type="checkbox" prop:checked=move || show_rejected.get()
                                    on:change=move |e| show_rejected.set(event_target_checked(&e)) />
                                "Show rejected"
                            </label>
                            <span class="isearch-count muted">
                                {(total > 0).then(|| format!("{total} found · {accepted} accepted"))}
                            </span>
                            <button type="button" class="btn btn-sm btn-secondary" disabled=move || loading.get()
                                on:click=move |_| run()>
                                <Icon name="refresh" />
                                "Search again"
                            </button>
                        }
                    }}
                </div>
                {table}
            </div>
        </Modal>
    }
}

fn release_row(
    r: ReleaseDto,
    grabbing: RwSignal<Option<String>>,
    grabbed: RwSignal<Vec<String>>,
    grab: Callback<(ReleaseDto, bool)>,
) -> impl IntoView {
    let key = StoredValue::new(format!("{}|{}", r.indexer_id, r.guid));
    let is_grabbing = move || key.with_value(|k| grabbing.with(|g| g.as_deref() == Some(k.as_str())));
    let is_grabbed = move || key.with_value(|k| grabbed.with(|g| g.contains(k)));
    let accepted = r.accepted;
    let reasons: Vec<String> = r.rejections.iter().map(|x| x.reason.clone()).collect();
    let tooltip = reasons.join("\n");
    let peers = match (r.protocol, r.seeders, r.peers) {
        (Protocol::Usenet, _, _) => "—".to_string(),
        (_, Some(s), Some(p)) => format!("{s} / {p}"),
        (_, Some(s), None) => s.to_string(),
        _ => "—".into(),
    };
    let eps = episodes_label(&r.episodes);
    let full_season = r.full_season;
    let rel = StoredValue::new(r.clone());
    let grab_label = if accepted { "Grab" } else { "Override and grab" };
    view! {
        <tr class:rejected=!accepted>
            <td><ProtoBadge protocol=r.protocol /></td>
            <td title=r.publish_date.map(crate::format::datetime).unwrap_or_default()>{age_label(r.age_minutes)}</td>
            <td class="release-title">
                {match r.info_url.as_deref().and_then(crate::format::http_url) {
                    Some(u) => view! { <a href=u target="_blank" rel="noopener noreferrer">{r.title.clone()}</a> }.into_any(),
                    None => view! { <span>{r.title.clone()}</span> }.into_any(),
                }}
                {(!accepted).then(|| view! {
                    <span class="reject-icon" title=tooltip.clone() aria-label=format!("Rejected: {}", reasons.join("; "))>
                        <Icon name="warning" />
                    </span>
                })}
                <span class="cell-sub release-sub">
                    {full_season.then(|| view! { <span class="chip chip-xs">"Season pack"</span> })}
                    {eps.map(|e| view! { <span>{e}</span> })}
                    {r.release_group.clone().map(|g| view! { <span>{g}</span> })}
                    {r.indexer_flags.iter().map(|f| view! { <span class="chip chip-xs">{f.replace('_', " ")}</span> }).collect_view()}
                </span>
                {(!accepted).then(|| view! {
                    <ul class="reject-list">
                        {reasons.iter().map(|x| view! { <li>{x.clone()}</li> }).collect_view()}
                    </ul>
                })}
            </td>
            <td>{r.indexer.clone()}</td>
            <td class="nowrap">{r.size.map(bytes).unwrap_or_else(|| "—".into())}</td>
            <td class="nowrap">{peers}</td>
            <td>{languages_label(&r.languages)}</td>
            <td><span class="badge tone-info">{quality_label(&r.quality)}</span></td>
            <td><FormatScore score=r.format_score formats=r.custom_formats.clone() /></td>
            <td class="mono">{r.rank}</td>
            <td class="cell-actions">
                {move || if is_grabbed() {
                    view! { <span class="badge tone-success"><Icon name="check" />"Sent"</span> }.into_any()
                } else {
                    view! {
                        <button type="button" class="icon-btn icon-btn-sm grab-btn" class:force=!accepted
                            title=grab_label aria-label=grab_label disabled=move || grabbing.with(Option::is_some)
                            on:click=move |_| grab.run((rel.get_value(), !accepted))>
                            {move || if is_grabbing() { view! { <Spinner label="Grabbing…" /> }.into_any() } else { view! { <Icon name="download" /> }.into_any() }}
                        </button>
                    }
                    .into_any()
                }}
            </td>
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_age() {
        assert_eq!(age_label(None), "—");
        assert_eq!(age_label(Some(5)), "5m");
        assert_eq!(age_label(Some(130)), "2h");
        assert_eq!(age_label(Some(3 * 1440)), "3d");
    }

    #[test]
    fn builds_release_urls() {
        assert_eq!(SearchTarget::movie(4, "x").url(), "/releases?movie_id=4");
        assert_eq!(
            SearchTarget::season(2, 3, "x").url(),
            "/releases?series_id=2&season=3"
        );
        assert_eq!(SearchTarget::episode(9, "x").url(), "/releases?episode_id=9");
        assert_eq!(
            SearchTarget::book(5, Some(BookFormat::Audiobook), "x").url(),
            "/releases?book_id=5&book_format=audiobook"
        );
        assert_eq!(SearchTarget::book(5, None, "x").url(), "/releases?book_id=5");
        assert_eq!(SearchTarget::album(8, "x").url(), "/releases?album_id=8");
    }
}
