//! Sonarr v3: series, episodes, episode files, lookup/add, calendar, wanted, history, commands.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use reelix_api_types::library::{
    AddSeriesRequest, CalendarKind, CalendarQuery, EpisodeDto, EpisodeFileDto, EpisodeMonitorRequest,
    LibrarySearchKind, LookupResult, NewItemMonitor, SearchRequest, SeasonMonitorRequest, SeriesDto,
    SeriesEdit, SeriesListQuery, SeriesMonitor, SeriesStatus, SeriesType, TagMode, WantedQuery,
};
use reelix_api_types::pipeline::HistoryQuery;
use serde::Deserialize;
use serde_json::{Value, json};

use super::radarr::parse_day;
use super::{
    App, CommandBody, DeleteQuery, PageQuery, Reply, activity, authorize, created, date, history_record,
    images, library, media_file, ok, paged, r_get, root_folder_id, run_task, unknown_command,
};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/series", r_get(series_list).post(add_series))
        .route("/series/lookup", r_get(lookup))
        .route(
            "/series/{id}",
            r_get(series).put(update_series).delete(delete_series),
        )
        .route("/episode", r_get(episodes))
        .route("/episode/monitor", axum::routing::put(monitor_episodes))
        .route("/episode/{id}", r_get(episode).put(update_episode))
        .route("/episodefile", r_get(episode_files))
        .route("/episodefile/{id}", r_get(episode_file))
        .route("/calendar", r_get(calendar))
        .route("/wanted/missing", r_get(wanted_missing))
        .route("/wanted/cutoff", r_get(wanted_cutoff))
        .route("/history", r_get(history))
        .route("/history/series", r_get(series_history))
        .route("/languageprofile", r_get(language_profiles))
}

fn status(s: SeriesStatus) -> &'static str {
    match s {
        SeriesStatus::Continuing => "continuing",
        SeriesStatus::Ended => "ended",
        SeriesStatus::Upcoming => "upcoming",
        _ => "deleted",
    }
}

fn series_type(t: SeriesType) -> &'static str {
    match t {
        SeriesType::Standard => "standard",
        SeriesType::Daily => "daily",
        SeriesType::Anime => "anime",
    }
}

fn parse_series_type(s: Option<&str>) -> Option<SeriesType> {
    Some(match s?.to_ascii_lowercase().as_str() {
        "daily" => SeriesType::Daily,
        "anime" => SeriesType::Anime,
        "standard" => SeriesType::Standard,
        _ => return None,
    })
}

fn slug(title: &str) -> String {
    let mut s: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.trim_matches('-').to_owned()
}

pub(super) fn series_json(s: &SeriesDto) -> Value {
    let root = std::path::Path::new(&s.path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let previous = s
        .seasons
        .iter()
        .filter_map(|x| x.statistics.previous_airing)
        .max();
    let seasons: Vec<Value> = s
        .seasons
        .iter()
        .map(|x| {
            let st = &x.statistics;
            let pct = if st.episode_count == 0 {
                0.0
            } else {
                f64::from(st.episode_file_count) * 100.0 / f64::from(st.episode_count)
            };
            json!({
                "seasonNumber": x.season_number,
                "monitored": x.monitored,
                "statistics": {
                    "previousAiring": st.previous_airing,
                    "nextAiring": st.next_airing,
                    "episodeFileCount": st.episode_file_count,
                    "episodeCount": st.episode_count,
                    "totalEpisodeCount": st.total_episode_count,
                    "sizeOnDisk": st.size_on_disk,
                    "releaseGroups": [],
                    "percentOfEpisodes": pct,
                },
            })
        })
        .collect();
    let st = &s.statistics;
    json!({
        "id": s.id,
        "title": s.title,
        "alternateTitles": s.alternative_titles.iter().map(|t| json!({"title": t, "seasonNumber": -1})).collect::<Vec<_>>(),
        "sortTitle": s.sort_title,
        "status": status(s.status),
        "ended": s.status == SeriesStatus::Ended,
        "overview": s.overview.clone().unwrap_or_default(),
        "previousAiring": previous,
        "nextAiring": date(s.next_airing),
        "network": s.network.clone().unwrap_or_default(),
        "images": images(&s.images.poster, &s.images.backdrop, &s.images.logo),
        "originalLanguage": {"id": 1, "name": s.original_language.clone().unwrap_or_default()},
        "seasons": seasons,
        "year": s.year.unwrap_or(0),
        "path": s.path,
        "qualityProfileId": s.quality_profile_id,
        "languageProfileId": 1,
        "seasonFolder": s.season_folder,
        "monitored": s.monitored,
        "monitorNewItems": if s.monitor_new_items == NewItemMonitor::None { "none" } else { "all" },
        "useSceneNumbering": false,
        "runtime": s.runtime_minutes.unwrap_or(0),
        "tvdbId": s.tvdb_id.unwrap_or(0),
        "tvRageId": 0,
        "tvMazeId": 0,
        "tmdbId": s.tmdb_id.unwrap_or(0),
        "firstAired": date(s.first_aired),
        "lastAired": date(s.last_aired),
        "seriesType": series_type(s.series_type),
        "cleanTitle": s.sort_title.replace(' ', ""),
        "imdbId": s.imdb_id.clone().unwrap_or_default(),
        "titleSlug": slug(&s.title),
        "rootFolderPath": root,
        "certification": s.certification.clone().unwrap_or_default(),
        "genres": s.genres,
        "tags": s.tags,
        "added": s.added,
        "ratings": {"votes": s.ratings.vote_count, "value": s.ratings.vote_average},
        "statistics": {
            "seasonCount": st.season_count,
            "episodeFileCount": st.episode_file_count,
            "episodeCount": st.episode_count,
            "totalEpisodeCount": st.total_episode_count,
            "sizeOnDisk": st.size_on_disk,
            "releaseGroups": [],
            "percentOfEpisodes": st.percent_of_episodes,
        },
    })
}

fn episode_file_json(f: &EpisodeFileDto) -> Value {
    let mut m = media_file(App::Sonarr, &f.file);
    m.insert("seriesId".into(), json!(f.series_id));
    m.insert("seasonNumber".into(), json!(f.season_number));
    Value::Object(m)
}

fn episode_json(e: &EpisodeDto, series: Option<&SeriesDto>) -> Value {
    let mut v = json!({
        "id": e.id,
        "seriesId": e.series_id,
        "tvdbId": 0,
        "episodeFileId": e.episode_file_id.unwrap_or(0),
        "seasonNumber": e.season_number,
        "episodeNumber": e.episode_number,
        "title": e.title.clone().unwrap_or_default(),
        "airDate": e.air_date.map(|d| d.to_string()),
        "airDateUtc": e.air_date_utc,
        "runtime": e.runtime_minutes.unwrap_or(0),
        "overview": e.overview.clone().unwrap_or_default(),
        "hasFile": e.has_file,
        "monitored": e.monitored,
        "absoluteEpisodeNumber": e.absolute_number,
        "sceneSeasonNumber": e.scene_season_number,
        "sceneEpisodeNumber": e.scene_episode_number,
        "sceneAbsoluteEpisodeNumber": e.scene_absolute_number,
        "unverifiedSceneNumbering": false,
        "finaleType": e.finale.clone(),
        "images": images(&e.still, &None, &None),
    });
    let m = v.as_object_mut().expect("object");
    if let Some(f) = &e.episode_file {
        m.insert("episodeFile".into(), episode_file_json(f));
    }
    if let Some(s) = series {
        m.insert("series".into(), series_json(s));
    }
    v
}

fn lookup_json(l: &LookupResult, existing: Option<&SeriesDto>) -> Value {
    if let Some(s) = existing {
        return series_json(s);
    }
    json!({
        "title": l.title,
        "sortTitle": l.title.to_lowercase(),
        "status": "continuing",
        "overview": l.overview.clone().unwrap_or_default(),
        "images": images(&l.poster, &l.backdrop, &None),
        "remotePoster": l.poster,
        "seasons": [],
        "year": l.year.unwrap_or(0),
        "monitored": false,
        "seasonFolder": true,
        "tvdbId": l.tvdb_id.unwrap_or(0),
        "tmdbId": l.tmdb_id.unwrap_or(0),
        "imdbId": l.imdb_id.clone().unwrap_or_default(),
        "titleSlug": slug(&l.title),
        "folder": l.title,
        "genres": [],
        "tags": [],
        "added": "0001-01-01T00:00:00Z",
        "ratings": {"votes": 0, "value": l.vote_average},
        "statistics": {"seasonCount": 0, "episodeFileCount": 0, "episodeCount": 0, "totalEpisodeCount": 0, "sizeOnDisk": 0, "percentOfEpisodes": 0.0},
    })
}

async fn all_series(state: &AppState) -> ApiResult<Vec<SeriesDto>> {
    Ok(library(state)?
        .list_series(&SeriesListQuery {
            page: Some(1),
            page_size: Some(0),
            ..Default::default()
        })
        .await?
        .items)
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeriesQuery {
    tvdb_id: Option<u32>,
}

async fn series_list(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<SeriesQuery>,
) -> Reply {
    authorize(&user)?;
    let list = all_series(&state).await?;
    ok(Value::Array(
        list.iter()
            .filter(|s| q.tvdb_id.is_none_or(|t| s.tvdb_id == Some(t)))
            .map(series_json)
            .collect(),
    ))
}

async fn series(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    ok(series_json(&library(&state)?.get_series(id).await?))
}

#[derive(Debug, Deserialize)]
struct TermQuery {
    term: Option<String>,
}

async fn lookup(State(state): State<AppState>, user: CurrentUser, Query(q): Query<TermQuery>) -> Reply {
    authorize(&user)?;
    let term = q.term.unwrap_or_default();
    if term.trim().is_empty() {
        return ok(json!([]));
    }
    let lib = library(&state)?;
    let results = lib.lookup_series(term.trim()).await?;
    let mut out = Vec::new();
    for r in &results {
        let existing = match r.library_id {
            Some(id) => lib.get_series(id).await.ok(),
            None => None,
        };
        out.push(lookup_json(r, existing.as_ref()));
    }
    ok(Value::Array(out))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeasonBody {
    season_number: u32,
    #[serde(default)]
    monitored: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddBody {
    tvdb_id: Option<u32>,
    tmdb_id: Option<u32>,
    /// Used only to find a series that the TMDB and TVDB ids do not lead to.
    imdb_id: Option<String>,
    quality_profile_id: i64,
    root_folder_path: Option<String>,
    path: Option<String>,
    #[serde(default = "yes")]
    season_folder: bool,
    #[serde(default = "yes")]
    monitored: bool,
    #[serde(default)]
    seasons: Vec<SeasonBody>,
    series_type: Option<String>,
    monitor_new_items: Option<String>,
    #[serde(default)]
    tags: Vec<i64>,
    #[serde(default)]
    add_options: AddOptions,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddOptions {
    monitor: Option<String>,
    #[serde(default)]
    search_for_missing_episodes: bool,
    #[serde(default)]
    search_for_cutoff_unmet_episodes: bool,
}

fn yes() -> bool {
    true
}

fn parse_monitor(s: Option<&str>) -> Option<SeriesMonitor> {
    Some(match s? {
        "all" => SeriesMonitor::All,
        "future" => SeriesMonitor::Future,
        "missing" => SeriesMonitor::Missing,
        "existing" => SeriesMonitor::Existing,
        "firstSeason" | "pilotSeason" => SeriesMonitor::FirstSeason,
        "latestSeason" | "lastSeason" => SeriesMonitor::LatestSeason,
        "pilot" => SeriesMonitor::Pilot,
        "none" => SeriesMonitor::None,
        _ => return None,
    })
}

async fn add_series(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(b): Json<AddBody>,
) -> ApiResult<Response> {
    authorize(&user)?;
    let (root_path, path) = match (&b.root_folder_path, &b.path) {
        (Some(r), _) if !r.trim().is_empty() => (r.clone(), None),
        (_, Some(p)) => (
            std::path::Path::new(p)
                .parent()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Some(p.clone()),
        ),
        _ => return Err(ApiError::validation("rootFolderPath is required")),
    };
    let root = root_folder_id(&state, App::Sonarr, &root_path).await?;
    // An explicit monitor option wins; otherwise the seasons list says what to monitor.
    let (monitor, seasons) = match parse_monitor(b.add_options.monitor.as_deref()) {
        Some(m) if m != SeriesMonitor::All || b.seasons.is_empty() => (m, None),
        _ if !b.monitored => (SeriesMonitor::None, None),
        _ if !b.seasons.is_empty() => (
            SeriesMonitor::All,
            Some(
                b.seasons
                    .iter()
                    .filter(|s| s.monitored)
                    .map(|s| s.season_number)
                    .collect(),
            ),
        ),
        _ => (SeriesMonitor::All, None),
    };
    let s = library(&state)?
        .add_series_by_any_id(
            AddSeriesRequest {
                tmdb_id: b.tmdb_id.filter(|t| *t > 0),
                tvdb_id: b.tvdb_id.filter(|t| *t > 0),
                quality_profile_id: b.quality_profile_id,
                root_folder_id: root,
                monitor,
                seasons,
                series_type: parse_series_type(b.series_type.as_deref()).unwrap_or_default(),
                season_folder: b.season_folder,
                monitor_new_items: if b.monitor_new_items.as_deref() == Some("none") {
                    NewItemMonitor::None
                } else {
                    NewItemMonitor::All
                },
                tags: b.tags,
                search_on_add: b.add_options.search_for_missing_episodes,
                search_cutoff_unmet: b.add_options.search_for_cutoff_unmet_episodes,
                path,
                added_by_request_id: None,
            },
            b.imdb_id.as_deref(),
        )
        .await?;
    created(series_json(&s))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBody {
    monitored: Option<bool>,
    quality_profile_id: Option<i64>,
    series_type: Option<String>,
    season_folder: Option<bool>,
    monitor_new_items: Option<String>,
    tags: Option<Vec<i64>>,
    path: Option<String>,
    #[serde(default)]
    seasons: Vec<SeasonBody>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveQuery {
    #[serde(default)]
    move_files: bool,
}

async fn update_series(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Query(mq): Query<MoveQuery>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Response> {
    authorize(&user)?;
    let lib = library(&state)?;
    let current = lib.get_series(id).await?;
    // Season monitoring changes (LunaSea/nzb360 toggle seasons by PUTting the whole series).
    for s in &b.seasons {
        let was = current
            .seasons
            .iter()
            .find(|x| x.season_number == s.season_number);
        if was.is_some_and(|w| w.monitored != s.monitored) {
            lib.monitor_season(SeasonMonitorRequest {
                series_id: id,
                season_number: s.season_number,
                monitored: s.monitored,
                include_episodes: true,
            })
            .await?;
        }
    }
    let updated = lib
        .update_series(
            id,
            SeriesEdit {
                monitored: b.monitored,
                quality_profile_id: b.quality_profile_id,
                series_type: parse_series_type(b.series_type.as_deref()),
                season_folder: b.season_folder,
                monitor_new_items: b.monitor_new_items.as_deref().map(|m| {
                    if m == "none" {
                        NewItemMonitor::None
                    } else {
                        NewItemMonitor::All
                    }
                }),
                root_folder_id: None,
                path: b
                    .path
                    .filter(|p| p.trim_end_matches('/') != current.path.trim_end_matches('/')),
                move_files: mq.move_files,
                tags: b.tags,
                tag_mode: TagMode::Replace,
            },
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(series_json(&updated))).into_response())
}

async fn delete_series(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<StatusCode> {
    authorize(&user)?;
    library(&state)?.delete_series(id, q.delete_files).await?;
    Ok(StatusCode::OK)
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpisodeQuery {
    series_id: Option<i64>,
    season_number: Option<u32>,
    #[serde(default)]
    episode_ids: Option<String>,
    #[serde(default)]
    include_series: bool,
    #[serde(default)]
    include_episode_file: bool,
}

async fn episodes(State(state): State<AppState>, user: CurrentUser, Query(q): Query<EpisodeQuery>) -> Reply {
    authorize(&user)?;
    let lib = library(&state)?;
    let Some(series_id) = q.series_id else {
        // `?episodeIds=1,2` without a series: search every series (small libraries only).
        let wanted: Vec<i64> = q
            .episode_ids
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if wanted.is_empty() {
            return Err(ApiError::validation("seriesId or episodeIds is required"));
        }
        let mut out = Vec::new();
        for s in all_series(&state).await? {
            for e in lib.episodes(s.id, None).await? {
                if wanted.contains(&e.id) {
                    out.push(episode_json(&e, q.include_series.then_some(&s)));
                }
            }
        }
        return ok(Value::Array(out));
    };
    let s = if q.include_series {
        Some(lib.get_series(series_id).await?)
    } else {
        None
    };
    let list = lib.episodes(series_id, q.season_number).await?;
    ok(Value::Array(
        list.iter()
            .map(|e| {
                let mut v = episode_json(e, s.as_ref());
                if !q.include_episode_file
                    && let Some(m) = v.as_object_mut()
                {
                    m.remove("episodeFile");
                }
                v
            })
            .collect(),
    ))
}

async fn find_episode(state: &AppState, id: i64) -> ApiResult<(EpisodeDto, SeriesDto)> {
    let lib = library(state)?;
    for s in all_series(state).await? {
        if let Some(e) = lib.episodes(s.id, None).await?.into_iter().find(|e| e.id == id) {
            return Ok((e, s));
        }
    }
    Err(ApiError::not_found("episode not found"))
}

async fn episode(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    let (e, s) = find_episode(&state, id).await?;
    ok(episode_json(&e, Some(&s)))
}

#[derive(Debug, Deserialize)]
struct EpisodeBody {
    monitored: bool,
}

async fn update_episode(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(b): Json<EpisodeBody>,
) -> ApiResult<Response> {
    authorize(&user)?;
    library(&state)?
        .monitor_episodes(EpisodeMonitorRequest {
            episode_ids: vec![id],
            monitored: b.monitored,
        })
        .await?;
    let (e, s) = find_episode(&state, id).await?;
    Ok((StatusCode::ACCEPTED, Json(episode_json(&e, Some(&s)))).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonitorBody {
    episode_ids: Vec<i64>,
    monitored: bool,
}

async fn monitor_episodes(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(b): Json<MonitorBody>,
) -> ApiResult<Response> {
    authorize(&user)?;
    let list = library(&state)?
        .monitor_episodes(EpisodeMonitorRequest {
            episode_ids: b.episode_ids,
            monitored: b.monitored,
        })
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Value::Array(list.iter().map(|e| episode_json(e, None)).collect())),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesQuery {
    series_id: Option<i64>,
}

async fn episode_files(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<FilesQuery>,
) -> Reply {
    authorize(&user)?;
    let id = q
        .series_id
        .ok_or_else(|| ApiError::validation("seriesId is required"))?;
    let files = library(&state)?.episode_files(id).await?;
    ok(Value::Array(files.iter().map(episode_file_json).collect()))
}

async fn episode_file(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    let lib = library(&state)?;
    for s in all_series(&state).await? {
        if let Some(f) = lib
            .episode_files(s.id)
            .await?
            .into_iter()
            .find(|f| f.file.id == id)
        {
            return ok(episode_file_json(&f));
        }
    }
    Err(ApiError::not_found("episode file not found"))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarParams {
    start: Option<String>,
    end: Option<String>,
    #[serde(default)]
    unmonitored: bool,
    #[serde(default)]
    include_series: bool,
}

/// Episodes by id for a set of `(series_id, episode_id)` pairs, with their series.
async fn episodes_for(state: &AppState, wanted: &[(i64, i64)], with_series: bool) -> ApiResult<Vec<Value>> {
    let lib = library(state)?;
    let mut by_series: HashMap<i64, (SeriesDto, Vec<EpisodeDto>)> = HashMap::new();
    let mut out = Vec::new();
    for (series_id, episode_id) in wanted {
        if !by_series.contains_key(series_id) {
            let s = lib.get_series(*series_id).await?;
            let eps = lib.episodes(*series_id, None).await?;
            by_series.insert(*series_id, (s, eps));
        }
        let (s, eps) = &by_series[series_id];
        if let Some(e) = eps.iter().find(|e| e.id == *episode_id) {
            out.push(episode_json(e, with_series.then_some(s)));
        }
    }
    Ok(out)
}

async fn calendar(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(p): Query<CalendarParams>,
) -> Reply {
    authorize(&user)?;
    let entries = library(&state)?
        .calendar(&CalendarQuery {
            start: parse_day(&p.start),
            end: parse_day(&p.end),
            unmonitored: p.unmonitored,
        })
        .await?;
    let wanted: Vec<(i64, i64)> = entries
        .iter()
        .filter(|e| e.kind == CalendarKind::Episode)
        .filter_map(|e| Some((e.series_id?, e.episode_id?)))
        .collect();
    ok(Value::Array(
        episodes_for(&state, &wanted, p.include_series).await?,
    ))
}

async fn wanted(state: &AppState, q: &PageQuery, cutoff: bool) -> Reply {
    let lib = library(state)?;
    let wq = WantedQuery {
        kind: Some(CalendarKind::Episode),
        page: q.page,
        page_size: q.page_size,
        monitored: q.monitored.or(Some(true)),
        ..Default::default()
    };
    let page = if cutoff {
        lib.wanted_cutoff(&wq).await?
    } else {
        lib.wanted_missing(&wq).await?
    };
    let pairs: Vec<(i64, i64)> = page
        .items
        .iter()
        .filter_map(|w| Some((w.series_id?, w.id)))
        .collect();
    let records = episodes_for(state, &pairs, q.include_series.unwrap_or(false)).await?;
    ok(paged(q, page.total, records))
}

async fn wanted_missing(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<PageQuery>,
) -> Reply {
    authorize(&user)?;
    wanted(&state, &q, false).await
}

async fn wanted_cutoff(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<PageQuery>,
) -> Reply {
    authorize(&user)?;
    wanted(&state, &q, true).await
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryParams {
    page: Option<u32>,
    page_size: Option<u32>,
    series_id: Option<i64>,
    episode_id: Option<i64>,
    event_type: Option<String>,
}

async fn history(State(state): State<AppState>, user: CurrentUser, Query(p): Query<HistoryParams>) -> Reply {
    authorize(&user)?;
    let page = activity(&state)?
        .history(&HistoryQuery {
            page: p.page,
            page_size: p.page_size,
            series_id: p.series_id,
            episode_id: p.episode_id,
            event_type: p.event_type.as_deref().and_then(super::history_event_from_arr),
            ..Default::default()
        })
        .await?;
    let records = page
        .items
        .iter()
        .filter(|h| h.media.as_ref().is_some_and(|m| m.kind == "series"))
        .map(|h| history_record(App::Sonarr, h))
        .collect();
    let q = PageQuery {
        page: p.page,
        page_size: p.page_size,
        ..Default::default()
    };
    ok(paged(&q, page.total, records))
}

async fn series_history(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(p): Query<HistoryParams>,
) -> Reply {
    authorize(&user)?;
    let id = p
        .series_id
        .ok_or_else(|| ApiError::validation("seriesId is required"))?;
    let page = activity(&state)?
        .history(&HistoryQuery {
            series_id: Some(id),
            page_size: Some(500),
            ..Default::default()
        })
        .await?;
    ok(Value::Array(
        page.items
            .iter()
            .map(|h| history_record(App::Sonarr, h))
            .collect(),
    ))
}

/// Sonarr v3 language profiles (gone in v4); one profile so v3 clients can add series.
async fn language_profiles(user: CurrentUser) -> Reply {
    authorize(&user)?;
    ok(json!([{
        "id": 1,
        "name": "Any",
        "upgradeAllowed": false,
        "cutoff": {"id": 1, "name": "English"},
        "languages": [{"language": {"id": 1, "name": "English"}, "allowed": true}],
    }]))
}

pub(super) async fn command(state: &AppState, b: &CommandBody) -> ApiResult<Option<i64>> {
    let lib = library(state)?;
    let mut series_ids = b.series_ids.clone();
    series_ids.extend(b.series_id);
    let search = |kind, ids, season| SearchRequest {
        kind,
        ids,
        season,
        media: None,
    };
    match b.name.as_str() {
        "SeriesSearch" => Ok(lib
            .request_search(search(LibrarySearchKind::Series, series_ids, None))
            .await?),
        "SeasonSearch" => {
            let season = b
                .season_number
                .ok_or_else(|| ApiError::validation("seasonNumber is required"))?;
            Ok(lib
                .request_search(search(LibrarySearchKind::Season, series_ids, Some(season)))
                .await?)
        }
        "EpisodeSearch" => Ok(lib
            .request_search(search(LibrarySearchKind::Episode, b.episode_ids.clone(), None))
            .await?),
        "MissingEpisodeSearch" => Ok(lib
            .request_search(SearchRequest {
                kind: LibrarySearchKind::Missing,
                ids: vec![],
                season: None,
                media: Some(CalendarKind::Episode),
            })
            .await?),
        "CutoffUnmetEpisodeSearch" => Ok(lib
            .request_search(SearchRequest {
                kind: LibrarySearchKind::Cutoff,
                ids: vec![],
                season: None,
                media: Some(CalendarKind::Episode),
            })
            .await?),
        "RefreshSeries" => {
            if series_ids.is_empty() {
                return run_task(state, reelix_library::kinds::REFRESH_SERIES).await;
            }
            let mut last = None;
            for id in series_ids {
                last = lib.queue_refresh_series(id).await?.or(last);
            }
            Ok(last)
        }
        "RescanSeries" => Ok(lib.queue_scan(&[], &series_ids).await?),
        "RssSync" => run_task(state, "rss_sync").await,
        "DownloadedEpisodesScan" | "ProcessMonitoredDownloads" => run_task(state, "download_monitor").await,
        other => Err(unknown_command(other)),
    }
}
