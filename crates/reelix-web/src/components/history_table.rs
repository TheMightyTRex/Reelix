//! History table (Activity → History and the History tabs of the detail pages), plus small
//! display helpers shared by the pipeline screens (queue, blocklist, interactive search).

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use reelix_api_types::pipeline::{EpisodeRefDto, FormatRefDto, HistoryDto, MediaRefDto, Paged};
use reelix_core::Protocol;
use reelix_parser::{Language, QualityModel};

use super::infinite::{InfiniteSentinel, PagedList};
use super::library_common::on_entity_changed;
use super::library_forms::with_query;
use super::{ConfirmDialog, EmptyState, ErrorBox, Icon, Skeleton, StatusBadge, Tone};
use crate::api;
use crate::format;
use crate::state::Toasts;

// ---------- shared display helpers ----------

/// `WEBDL-1080p`, `Bluray-2160p Proper`, `HDTV-720p v3`.
pub fn quality_label(q: &QualityModel) -> String {
    let name = q.quality.name();
    let r = q.revision;
    let mut out = name.to_string();
    if r.version == 2 {
        out.push_str(if r.is_repack { " Repack" } else { " Proper" });
    } else if r.version > 2 {
        out.push_str(&format!(" v{}", r.version));
    }
    if r.real > 0 {
        out.push_str(" REAL");
    }
    out
}

/// `torrent` / `usenet`.
pub fn proto_key(p: Protocol) -> &'static str {
    match p {
        Protocol::Torrent => "torrent",
        Protocol::Usenet => "usenet",
    }
}

/// Small protocol pill.
#[component]
pub fn ProtoBadge(protocol: Protocol) -> impl IntoView {
    let key = proto_key(protocol);
    view! { <span class=format!("badge proto-badge proto-{key}")>{key}</span> }
}

/// `S01E02`.
pub fn episode_code(e: &EpisodeRefDto) -> String {
    format!("S{:02}E{:02}", e.season_number, e.episode_number)
}

/// `S01E01-E03` style summary of a list of episodes (`Season 1` for many).
pub fn episodes_label(eps: &[EpisodeRefDto]) -> Option<String> {
    match eps {
        [] => None,
        [one] => Some(episode_code(one)),
        [first, .., last] if eps.len() > 4 && first.season_number == last.season_number => {
            Some(format!("Season {} ({} episodes)", first.season_number, eps.len()))
        }
        [first, .., last] if first.season_number == last.season_number => {
            Some(format!("{}-E{:02}", episode_code(first), last.episode_number))
        }
        _ => Some(eps.iter().map(episode_code).collect::<Vec<_>>().join(" ")),
    }
}

/// Library link for a media ref: `/movies/{id}`, `/series/{id}`, `/books/book/{id}`,
/// `/music/albums/{id}` or a radio episode's show (`/radio/shows/{show_id}`, else `/radio`).
pub fn media_href(m: &MediaRefDto) -> String {
    match m.kind.as_str() {
        "radio_episode" => m
            .show_id
            .map_or_else(|| "/radio".to_string(), |id| format!("/radio/shows/{id}")),
        "series" => format!("/series/{}", m.id),
        "book" => format!("/books/book/{}", m.id),
        "album" => format!("/music/albums/{}", m.id),
        _ => format!("/movies/{}", m.id),
    }
}

/// `by Author · Ebook` / `by Artist` line for books and albums; the show of a radio episode.
pub fn media_subtitle(m: &MediaRefDto) -> Option<String> {
    if m.kind == "radio_episode" {
        return m.subtitle.clone().filter(|s| !s.is_empty());
    }
    let format = m.book_format.map(|f| match f {
        reelix_core::BookFormat::Ebook => "Ebook",
        reelix_core::BookFormat::Audiobook => "Audiobook",
    });
    let by = m
        .subtitle
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| format!("by {s}"));
    match (by, format) {
        (None, None) => None,
        (Some(b), None) => Some(b),
        (None, Some(f)) => Some(f.to_string()),
        (Some(b), Some(f)) => Some(format!("{b} · {f}")),
    }
}

/// `Title (2024)`; a radio episode as `S20E03 - Title`.
pub fn media_title(m: &MediaRefDto) -> String {
    match m.year {
        Some(y) if m.kind != "radio_episode" => format!("{} ({y})", m.title),
        _ => m.title.clone(),
    }
}

/// Icon shown in front of rows that are not films or TV (radio episodes).
fn media_icon(m: &MediaRefDto) -> Option<&'static str> {
    (m.kind == "radio_episode").then_some("radio")
}

/// Media link plus optional episode summary underneath.
#[component]
pub fn MediaCell(
    media: Option<MediaRefDto>,
    #[prop(optional)] episodes: Vec<EpisodeRefDto>,
) -> impl IntoView {
    let eps = episodes_label(&episodes);
    let ep_title = (episodes.len() == 1).then(|| episodes[0].title.clone()).flatten();
    match media {
        None => view! { <span class="muted">"Unknown"</span> }.into_any(),
        Some(m) => {
            let sub = media_subtitle(&m);
            let icon = media_icon(&m);
            view! {
                {icon.map(|i| view! { <span class="media-kind-icon" title="Radio" aria-hidden="true"><Icon name=i /></span> })}
                <A href=media_href(&m) attr:class="cell-strong">{media_title(&m)}</A>
                {sub.map(|s| view! { <span class="cell-sub">{s}</span> })}
                {eps.map(|e| view! {
                    <span class="cell-sub">{e}{ep_title.map(|t| format!(" · {t}"))}</span>
                })}
            }
            .into_any()
        }
    }
}

/// Custom-format score with the format names as a tooltip.
#[component]
pub fn FormatScore(score: i32, formats: Vec<FormatRefDto>) -> impl IntoView {
    let names = formats
        .iter()
        .map(|f| f.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let tone = if score > 0 {
        "score-pos"
    } else if score < 0 {
        "score-neg"
    } else {
        "score-zero"
    };
    let title = if names.is_empty() {
        "No custom formats".to_string()
    } else {
        names
    };
    view! {
        <span class=format!("cf-score {tone}") title=title.clone() aria-label=format!("Score {score:+}: {title}")>
            {format!("{score:+}")}
        </span>
    }
}

/// `English, French`.
pub fn languages_label(langs: &[Language]) -> String {
    if langs.is_empty() {
        return "—".into();
    }
    langs.iter().map(|l| l.name()).collect::<Vec<_>>().join(", ")
}

/// Label + tone of a history event type.
pub fn event_label(event: &str) -> (Tone, &'static str, &'static str) {
    match event {
        "grabbed" => (Tone::Info, "Grabbed", "download"),
        "imported" => (Tone::Success, "Imported", "check"),
        "download_failed" => (Tone::Danger, "Download failed", "error"),
        "import_failed" => (Tone::Danger, "Import failed", "error"),
        "file_deleted" => (Tone::Warning, "File deleted", "trash"),
        "renamed" => (Tone::Neutral, "Renamed", "folder"),
        "ignored" => (Tone::Neutral, "Ignored", "close"),
        _ => (Tone::Neutral, "Event", "info"),
    }
}

/// History event types with labels, for filters.
pub const EVENT_TYPES: [(&str, &str); 8] = [
    ("", "All events"),
    ("grabbed", "Grabbed"),
    ("imported", "Imported"),
    ("download_failed", "Download failed"),
    ("import_failed", "Import failed"),
    ("file_deleted", "File deleted"),
    ("renamed", "Renamed"),
    ("ignored", "Ignored"),
];

/// `key → value` rows for a history entry's JSON `data` (nested values shown as JSON).
fn data_rows(data: &serde_json::Value) -> Vec<(String, String)> {
    match data {
        serde_json::Value::Object(map) => map
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| {
                let text = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (format::humanize_kind(k), text)
            })
            .collect(),
        serde_json::Value::Null => Vec::new(),
        other => vec![("Data".into(), other.to_string())],
    }
}

// ---------- history table ----------

/// History with an event-type filter, loading more rows as it scrolls. Scoped to one movie / series / episode when an id is
/// given (the media column is then hidden for movies).
#[component]
pub fn HistoryTable(
    #[prop(optional)] movie_id: Option<i64>,
    #[prop(optional)] series_id: Option<i64>,
    #[prop(optional)] episode_id: Option<i64>,
    #[prop(default = 25)] page_size: u32,
    /// Shared event-type filter (the History page renders its own select into the header).
    #[prop(optional)]
    event_type: Option<RwSignal<String>>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let own_filter = event_type.is_none();
    let event_type = event_type.unwrap_or_else(|| RwSignal::new(String::new()));
    let expanded = RwSignal::new(Vec::<i64>::new());
    let fail_target = RwSignal::new(None::<HistoryDto>);
    let fail_open = RwSignal::new(false);
    let show_media = movie_id.is_none();

    let history = PagedList::new(
        page_size,
        |h: &HistoryDto| h.id,
        move |page, size| {
            let url = with_query(
                "/history",
                &[
                    ("page", page.to_string()),
                    ("page_size", size.to_string()),
                    ("movie_id", movie_id.map(|v| v.to_string()).unwrap_or_default()),
                    ("series_id", series_id.map(|v| v.to_string()).unwrap_or_default()),
                    (
                        "episode_id",
                        episode_id.map(|v| v.to_string()).unwrap_or_default(),
                    ),
                    ("event_type", event_type.get_untracked()),
                ],
            );
            async move {
                api::get::<Paged<HistoryDto>>(&url)
                    .await
                    .map(|p| (p.items, p.total))
            }
        },
    );
    // A new filter starts over from the newest entries.
    Effect::new(move |_| {
        event_type.track();
        history.reset();
    });
    on_entity_changed(&["history"], false, move || history.refresh());

    let mark_failed = Callback::new(move |()| {
        let Some(h) = fail_target.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::post_empty::<serde_json::Value>(&format!("/history/{}/failed", h.id)).await {
                Ok(_) => {
                    toasts
                        .success("Marked as failed. The release is blocklisted and a new search is queued.");
                    history.refresh();
                }
                Err(e) => toasts.error(format!("Could not mark as failed: {e}")),
            }
        });
    });

    let body = move || {
        if !history.ready.get() {
            return view! { <Skeleton lines=4 /> }.into_any();
        }
        let items = history.items.get();
        if items.is_empty() {
            return match history.error.get() {
                Some(e) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| history.reset()) /> }.into_any(),
                None => view! {
                    <EmptyState icon="clock" title="No history yet" compact=true
                        description="Grabs, imports, upgrades and failures are recorded here as they happen." />
                }
                .into_any(),
            };
        }
        {
            let cols = if show_media { 8 } else { 7 };
            view! {
                <div class="table-wrap">
                    <table class="table history-table">
                        <caption class="visually-hidden">"History"</caption>
                        <thead>
                            <tr>
                                <th scope="col">"Event"</th>
                                {show_media.then(|| view! { <th scope="col">"Media"</th> })}
                                <th scope="col">"Release"</th>
                                <th scope="col">"Quality"</th>
                                <th scope="col">"Score"</th>
                                <th scope="col">"Source"</th>
                                <th scope="col">"Date"</th>
                                <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                            </tr>
                        </thead>
                        <tbody>
                            {items.into_iter().map(|h| {
                                let hid = h.id;
                                let open = move || expanded.with(|e| e.contains(&hid));
                                let (tone, label, _) = event_label(&h.event_type);
                                let eps: Vec<EpisodeRefDto> = h.episode.clone().into_iter().collect();
                                let ep_only = (!show_media).then(|| episodes_label(&eps)).flatten();
                                let source = [h.indexer.clone(), h.download_client.clone()]
                                    .into_iter()
                                    .flatten()
                                    .collect::<Vec<_>>()
                                    .join(" → ");
                                let can_fail = h.event_type == "grabbed";
                                let detail = h.clone();
                                let h_fail = h.clone();
                                view! {
                                    <tr class:is-open=open>
                                        <td><StatusBadge tone=tone label=label /></td>
                                        {show_media.then(|| view! { <td><MediaCell media=h.media.clone() episodes=eps.clone() /></td> })}
                                        <td class="release-title">
                                            <span class="mono">{h.source_title.clone()}</span>
                                            {ep_only.map(|e| view! { <span class="cell-sub">{e}</span> })}
                                        </td>
                                        <td>{h.quality.as_ref().map(quality_label).unwrap_or_else(|| "—".into())}</td>
                                        <td><FormatScore score=h.format_score formats=h.custom_formats.clone() /></td>
                                        <td>{if source.is_empty() { "—".to_string() } else { source }}</td>
                                        <td title=format::datetime(h.date)>{format::relative(h.date)}</td>
                                        <td class="cell-actions">
                                            <button type="button" class="btn btn-sm btn-ghost" aria-expanded=move || open().to_string()
                                                title="Details" on:click=move |_| super::library_common::toggle_id(expanded, hid)>
                                                <Icon name="info" />
                                                <span class="visually-hidden">"Details"</span>
                                            </button>
                                            {can_fail.then(|| view! {
                                                <button type="button" class="btn btn-sm btn-ghost btn-danger-text" title="Mark as failed"
                                                    on:click=move |_| { fail_target.set(Some(h_fail.clone())); fail_open.set(true); }>
                                                    <Icon name="error" />
                                                    <span class="visually-hidden">"Mark as failed"</span>
                                                </button>
                                            })}
                                        </td>
                                    </tr>
                                    <Show when=open>
                                        <tr class="history-detail">
                                            <td colspan=cols>
                                                <HistoryDetail entry=detail.clone() />
                                            </td>
                                        </tr>
                                    </Show>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
                {move || history.error.get().map(|e| view! { <ErrorBox error=e on_retry=Callback::new(move |()| history.retry()) /> })}
            }
            .into_any()
        }
    };

    view! {
        {own_filter.then(|| view! {
            <div class="toolbar">
                <EventTypeSelect value=event_type id="ht-event" />
            </div>
        })}
        {body}
        <InfiniteSentinel
            on_visible=Callback::new(move |()| history.load_more())
            enabled=Signal::derive(move || history.can_load_more())
            loading=history.loading
            loaded=Signal::derive(move || history.loaded())
            total=history.total
        />
        <ConfirmDialog open=fail_open title="Mark as failed?"
            message=Signal::derive(move || fail_target.get().map(|h| format!(
                "“{}” is removed, blocklisted, and a new search starts for the same media.", h.source_title
            )).unwrap_or_default())
            confirm_label="Mark failed" on_confirm=mark_failed />
    }
}

/// Event-type filter select.
#[component]
pub fn EventTypeSelect(value: RwSignal<String>, id: &'static str) -> impl IntoView {
    view! {
        <label class="visually-hidden" for=id>"Event type"</label>
        <select id=id class="input" on:change=move |e| value.set(event_target_value(&e))
            prop:value=move || value.get()>
            {EVENT_TYPES.iter().map(|(v, l)| view! { <option value=*v>{*l}</option> }).collect_view()}
        </select>
    }
}

/// Expanded details of one history entry.
#[component]
fn HistoryDetail(entry: HistoryDto) -> impl IntoView {
    let mut rows: Vec<(String, String)> = vec![
        ("Date".into(), format::datetime(entry.date)),
        ("Languages".into(), languages_label(&entry.languages)),
    ];
    if !entry.custom_formats.is_empty() {
        rows.push((
            "Custom formats".into(),
            entry
                .custom_formats
                .iter()
                .map(|f| f.name.clone())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let Some(i) = &entry.indexer {
        rows.push(("Indexer".into(), i.clone()));
    }
    if let Some(c) = &entry.download_client {
        rows.push(("Download client".into(), c.clone()));
    }
    if let Some(d) = &entry.download_id {
        rows.push(("Download id".into(), d.clone()));
    }
    if let Some(e) = &entry.episode {
        rows.push((
            "Episode".into(),
            match &e.title {
                Some(t) => format!("{} · {t}", episode_code(e)),
                None => episode_code(e),
            },
        ));
    }
    rows.extend(data_rows(&entry.data));
    view! {
        <dl class="kv detail-kv">
            {rows.into_iter().map(|(k, v)| view! {
                <div class="kv-row">
                    <dt>{k}</dt>
                    <dd class="mono">{v}</dd>
                </div>
            }).collect_view()}
        </dl>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelix_parser::Quality;

    fn ep(s: u32, e: u32) -> EpisodeRefDto {
        EpisodeRefDto {
            id: i64::from(s * 100 + e),
            season_number: s,
            episode_number: e,
            title: None,
        }
    }

    #[test]
    fn links_books_and_albums() {
        let mut m = MediaRefDto {
            kind: "book".into(),
            id: 4,
            title: "Dune".into(),
            year: Some(1965),
            subtitle: Some("Frank Herbert".into()),
            book_format: Some(reelix_core::BookFormat::Audiobook),
            show_id: None,
        };
        assert_eq!(media_href(&m), "/books/book/4");
        assert_eq!(
            media_subtitle(&m).as_deref(),
            Some("by Frank Herbert · Audiobook")
        );
        m.kind = "album".into();
        m.book_format = None;
        assert_eq!(media_href(&m), "/music/albums/4");
        assert_eq!(media_subtitle(&m).as_deref(), Some("by Frank Herbert"));
        m.kind = "series".into();
        m.subtitle = None;
        assert_eq!(media_href(&m), "/series/4");
        assert_eq!(media_subtitle(&m), None);
    }

    #[test]
    fn links_radio_episodes_to_their_show() {
        let mut m = MediaRefDto {
            kind: "radio_episode".into(),
            id: 31,
            title: "S20E03 - The Fete".into(),
            year: Some(2026),
            subtitle: Some("The Archers".into()),
            book_format: None,
            show_id: Some(7),
        };
        assert_eq!(media_href(&m), "/radio/shows/7");
        assert_eq!(media_title(&m), "S20E03 - The Fete");
        assert_eq!(media_subtitle(&m).as_deref(), Some("The Archers"));
        assert_eq!(media_icon(&m), Some("radio"));
        m.show_id = None;
        assert_eq!(media_href(&m), "/radio");
    }

    #[test]
    fn labels_episodes() {
        assert_eq!(episodes_label(&[]), None);
        assert_eq!(episodes_label(&[ep(1, 2)]).as_deref(), Some("S01E02"));
        assert_eq!(
            episodes_label(&[ep(1, 2), ep(1, 3)]).as_deref(),
            Some("S01E02-E03")
        );
        let season: Vec<_> = (1..=8).map(|e| ep(2, e)).collect();
        assert_eq!(episodes_label(&season).as_deref(), Some("Season 2 (8 episodes)"));
    }

    #[test]
    fn labels_quality_revisions() {
        let mut q = QualityModel::new(Quality::ALL[1]);
        let base = quality_label(&q);
        q.revision.version = 2;
        assert_eq!(quality_label(&q), format!("{base} Proper"));
        q.revision.is_repack = true;
        assert_eq!(quality_label(&q), format!("{base} Repack"));
    }

    #[test]
    fn flattens_history_data() {
        let rows =
            data_rows(&serde_json::json!({"destination_path": "/m/a.mkv", "is_upgrade": true, "x": null}));
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|(k, v)| k == "Destination path" && v == "/m/a.mkv")
        );
    }
}
