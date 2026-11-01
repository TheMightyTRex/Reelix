//! Radarr v3: movies, movie files, lookup/add, calendar, wanted, history, commands.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use reelix_api_types::library::{
    AddMovieRequest, CalendarKind, CalendarQuery, LibrarySearchKind, LookupResult, MinimumAvailability,
    MovieDto, MovieEdit, MovieFileDto, MovieListQuery, MovieMonitor, MovieStatus, SearchRequest, TagMode,
    WantedQuery,
};
use reelix_api_types::pipeline::HistoryQuery;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    App, CommandBody, DeleteQuery, PageQuery, Reply, activity, authorize, created, date, history_record,
    images, library, media_file, ok, paged, r_get, root_folder_id, run_task, unknown_command,
};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/movie", r_get(movies).post(add_movie))
        .route("/movie/lookup", r_get(lookup))
        .route("/movie/lookup/tmdb", r_get(lookup_tmdb))
        .route("/movie/lookup/imdb", r_get(lookup_imdb))
        .route("/movie/{id}", r_get(movie).put(update_movie).delete(delete_movie))
        .route("/moviefile", r_get(movie_files))
        .route("/moviefile/{id}", r_get(movie_file))
        .route("/calendar", r_get(calendar))
        .route("/wanted/missing", r_get(wanted_missing))
        .route("/wanted/cutoff", r_get(wanted_cutoff))
        .route("/history", r_get(history))
        .route("/history/movie", r_get(movie_history))
}

fn status(s: MovieStatus) -> &'static str {
    match s {
        MovieStatus::Tba => "tba",
        MovieStatus::Announced => "announced",
        MovieStatus::InCinemas => "inCinemas",
        MovieStatus::Released => "released",
        MovieStatus::Deleted => "deleted",
    }
}

fn availability(a: MinimumAvailability) -> &'static str {
    match a {
        MinimumAvailability::Announced => "announced",
        MinimumAvailability::InCinemas => "inCinemas",
        MinimumAvailability::Released => "released",
    }
}

fn parse_availability(s: Option<&str>) -> Option<MinimumAvailability> {
    Some(match s?.to_ascii_lowercase().as_str() {
        "tba" | "announced" => MinimumAvailability::Announced,
        "incinemas" => MinimumAvailability::InCinemas,
        "released" | "predb" => MinimumAvailability::Released,
        _ => return None,
    })
}

fn slug(title: &str, tmdb: u32) -> String {
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
    format!("{}-{tmdb}", s.trim_matches('-'))
}

fn movie_file_json(f: &MovieFileDto) -> Value {
    let mut m = media_file(App::Radarr, &f.file);
    m.insert("movieId".into(), json!(f.movie_id));
    Value::Object(m)
}

pub(super) fn movie_json(m: &MovieDto) -> Value {
    let root = std::path::Path::new(&m.path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = std::path::Path::new(&m.path)
        .file_name()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    json!({
        "id": m.id,
        "title": m.title,
        "originalTitle": m.original_title.clone().unwrap_or_else(|| m.title.clone()),
        "originalLanguage": {"id": 1, "name": m.original_language.clone().unwrap_or_default()},
        "alternateTitles": m.alternative_titles.iter().map(|t| json!({"title": t})).collect::<Vec<_>>(),
        "sortTitle": m.sort_title,
        "sizeOnDisk": m.size_on_disk,
        "status": status(m.status),
        "overview": m.overview.clone().unwrap_or_default(),
        "inCinemas": date(m.in_cinemas),
        "physicalRelease": date(m.physical_release),
        "digitalRelease": date(m.digital_release),
        "images": images(&m.images.poster, &m.images.backdrop, &m.images.logo),
        "website": m.homepage.clone().unwrap_or_default(),
        "year": m.year.unwrap_or(0),
        "youTubeTrailerId": m.youtube_trailer.clone().unwrap_or_default(),
        "studio": m.studio.clone().unwrap_or_default(),
        "path": m.path,
        "qualityProfileId": m.quality_profile_id,
        "hasFile": m.has_file,
        "movieFileId": m.movie_file.as_ref().map_or(0, |f| f.file.id),
        "monitored": m.monitored,
        "minimumAvailability": availability(m.minimum_availability),
        "isAvailable": m.is_available,
        "folderName": folder,
        "runtime": m.runtime_minutes.unwrap_or(0),
        "cleanTitle": m.sort_title.replace(' ', ""),
        "imdbId": m.imdb_id.clone().unwrap_or_default(),
        "tmdbId": m.tmdb_id,
        "titleSlug": slug(&m.title, m.tmdb_id),
        "rootFolderPath": root,
        "certification": m.certification.clone().unwrap_or_default(),
        "genres": m.genres,
        "tags": m.tags,
        "added": m.added,
        "ratings": {"tmdb": {"votes": m.ratings.vote_count, "value": m.ratings.vote_average, "type": "user"}},
        "movieFile": m.movie_file.as_ref().map(movie_file_json),
        "collection": m.collection.as_ref().map(|c| json!({"title": c.title, "tmdbId": c.tmdb_id})),
        "popularity": m.ratings.popularity,
        "statistics": {
            "movieFileCount": u32::from(m.has_file),
            "sizeOnDisk": m.size_on_disk,
            "releaseGroups": [],
        },
    })
}

fn lookup_json(l: &LookupResult, existing: Option<&MovieDto>) -> Value {
    if let Some(m) = existing {
        return movie_json(m);
    }
    let tmdb = l.tmdb_id.unwrap_or(0);
    json!({
        "id": 0,
        "title": l.title,
        "originalTitle": l.original_title.clone().unwrap_or_else(|| l.title.clone()),
        "sortTitle": l.title.to_lowercase(),
        "status": "released",
        "overview": l.overview.clone().unwrap_or_default(),
        "images": images(&l.poster, &l.backdrop, &None),
        "remotePoster": l.poster,
        "year": l.year.unwrap_or(0),
        "hasFile": false,
        "monitored": false,
        "isAvailable": true,
        "folder": format!("{} ({})", l.title, l.year.unwrap_or(0)),
        "imdbId": l.imdb_id.clone().unwrap_or_default(),
        "tmdbId": tmdb,
        "titleSlug": slug(&l.title, tmdb),
        "genres": [],
        "tags": [],
        "ratings": {"tmdb": {"votes": 0, "value": l.vote_average, "type": "user"}},
        "popularity": l.popularity,
        "added": "0001-01-01T00:00:00Z",
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MovieQuery {
    tmdb_id: Option<u32>,
}

async fn all_movies(state: &AppState) -> ApiResult<Vec<MovieDto>> {
    Ok(library(state)?
        .list_movies(&MovieListQuery {
            page: Some(1),
            page_size: Some(0),
            ..Default::default()
        })
        .await?
        .items)
}

async fn movies(State(state): State<AppState>, user: CurrentUser, Query(q): Query<MovieQuery>) -> Reply {
    authorize(&user)?;
    if let Some(tmdb) = q.tmdb_id {
        let m = library(&state)?.movie_by_tmdb(tmdb).await?;
        return ok(Value::Array(m.iter().map(movie_json).collect()));
    }
    let list = all_movies(&state).await?;
    ok(Value::Array(list.iter().map(movie_json).collect()))
}

async fn movie(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    ok(movie_json(&library(&state)?.get_movie(id).await?))
}

#[derive(Debug, Deserialize)]
struct TermQuery {
    term: Option<String>,
    #[serde(rename = "tmdbId")]
    tmdb_id: Option<u32>,
    #[serde(rename = "imdbId")]
    imdb_id: Option<String>,
}

async fn lookup_term(state: &AppState, term: &str) -> ApiResult<Vec<Value>> {
    let lib = library(state)?;
    let results = lib.lookup_movies(term).await?;
    let mut out = Vec::new();
    for r in &results {
        let existing = match r.library_id {
            Some(id) => lib.get_movie(id).await.ok(),
            None => None,
        };
        out.push(lookup_json(r, existing.as_ref()));
    }
    Ok(out)
}

async fn lookup(State(state): State<AppState>, user: CurrentUser, Query(q): Query<TermQuery>) -> Reply {
    authorize(&user)?;
    let term = q.term.unwrap_or_default();
    if term.trim().is_empty() {
        return ok(json!([]));
    }
    ok(Value::Array(lookup_term(&state, term.trim()).await?))
}

async fn lookup_tmdb(State(state): State<AppState>, user: CurrentUser, Query(q): Query<TermQuery>) -> Reply {
    authorize(&user)?;
    let id = q
        .tmdb_id
        .ok_or_else(|| ApiError::validation("tmdbId is required"))?;
    lookup_term(&state, &format!("tmdb:{id}"))
        .await?
        .into_iter()
        .next()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("movie not found"))
}

async fn lookup_imdb(State(state): State<AppState>, user: CurrentUser, Query(q): Query<TermQuery>) -> Reply {
    authorize(&user)?;
    let id = q
        .imdb_id
        .ok_or_else(|| ApiError::validation("imdbId is required"))?;
    lookup_term(&state, &format!("imdb:{id}"))
        .await?
        .into_iter()
        .next()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("movie not found"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddBody {
    tmdb_id: u32,
    quality_profile_id: i64,
    root_folder_path: Option<String>,
    path: Option<String>,
    #[serde(default = "yes")]
    monitored: bool,
    minimum_availability: Option<String>,
    #[serde(default)]
    tags: Vec<i64>,
    #[serde(default)]
    add_options: AddOptions,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddOptions {
    #[serde(default)]
    search_for_movie: bool,
    monitor: Option<String>,
}

fn yes() -> bool {
    true
}

async fn add_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(b): Json<AddBody>,
) -> ApiResult<Response> {
    authorize(&user)?;
    // Radarr accepts either the root folder or the full movie path.
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
    let root = root_folder_id(&state, App::Radarr, &root_path).await?;
    let monitor = match b.add_options.monitor.as_deref() {
        Some("none") => MovieMonitor::None,
        Some("movieAndCollection") => MovieMonitor::MovieAndCollection,
        _ if !b.monitored => MovieMonitor::None,
        _ => MovieMonitor::MovieOnly,
    };
    let m = library(&state)?
        .add_movie(AddMovieRequest {
            tmdb_id: b.tmdb_id,
            quality_profile_id: b.quality_profile_id,
            root_folder_id: root,
            monitor,
            minimum_availability: parse_availability(b.minimum_availability.as_deref()).unwrap_or_default(),
            tags: b.tags,
            search_on_add: b.add_options.search_for_movie,
            path,
            added_by_request_id: None,
        })
        .await?;
    created(movie_json(&m))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBody {
    monitored: Option<bool>,
    quality_profile_id: Option<i64>,
    minimum_availability: Option<String>,
    tags: Option<Vec<i64>>,
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveQuery {
    #[serde(default)]
    move_files: bool,
}

async fn update_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Query(mq): Query<MoveQuery>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Response> {
    authorize(&user)?;
    let lib = library(&state)?;
    let current = lib.get_movie(id).await?;
    let m = lib
        .update_movie(
            id,
            MovieEdit {
                monitored: b.monitored,
                quality_profile_id: b.quality_profile_id,
                minimum_availability: parse_availability(b.minimum_availability.as_deref()),
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
    Ok((StatusCode::ACCEPTED, Json(movie_json(&m))).into_response())
}

async fn delete_movie(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<StatusCode> {
    authorize(&user)?;
    library(&state)?.delete_movie(id, q.delete_files).await?;
    Ok(StatusCode::OK)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesQuery {
    movie_id: Option<i64>,
}

async fn movie_files(State(state): State<AppState>, user: CurrentUser, Query(q): Query<FilesQuery>) -> Reply {
    authorize(&user)?;
    let lib = library(&state)?;
    let files = match q.movie_id {
        Some(id) => lib.movie_files(id).await?,
        None => {
            let mut all = Vec::new();
            for m in all_movies(&state).await?.into_iter().filter(|m| m.has_file) {
                all.extend(lib.movie_files(m.id).await?);
            }
            all
        }
    };
    ok(Value::Array(files.iter().map(movie_file_json).collect()))
}

async fn movie_file(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    let lib = library(&state)?;
    for m in all_movies(&state).await?.into_iter().filter(|m| m.has_file) {
        if let Some(f) = lib.movie_files(m.id).await?.into_iter().find(|f| f.file.id == id) {
            return ok(movie_file_json(&f));
        }
    }
    Err(ApiError::not_found("movie file not found"))
}

#[derive(Debug, Default, Deserialize)]
struct CalendarParams {
    start: Option<String>,
    end: Option<String>,
    #[serde(default)]
    unmonitored: bool,
}

pub(super) fn parse_day(s: &Option<String>) -> Option<chrono::NaiveDate> {
    let s = s.as_deref()?.trim();
    chrono::NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

async fn calendar(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(p): Query<CalendarParams>,
) -> Reply {
    authorize(&user)?;
    let lib = library(&state)?;
    let entries = lib
        .calendar(&CalendarQuery {
            start: parse_day(&p.start),
            end: parse_day(&p.end),
            unmonitored: p.unmonitored,
        })
        .await?;
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for e in entries.into_iter().filter(|e| e.kind == CalendarKind::Movie) {
        if let Some(id) = e.movie_id
            && seen.insert(id)
            && let Ok(m) = lib.get_movie(id).await
        {
            out.push(movie_json(&m));
        }
    }
    ok(Value::Array(out))
}

async fn wanted(state: &AppState, q: &PageQuery, cutoff: bool) -> Reply {
    let lib = library(state)?;
    let wq = WantedQuery {
        kind: Some(CalendarKind::Movie),
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
    let mut records = Vec::new();
    for w in &page.items {
        if let Some(id) = w.movie_id
            && let Ok(m) = lib.get_movie(id).await
        {
            records.push(movie_json(&m));
        }
    }
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
    movie_id: Option<i64>,
    event_type: Option<String>,
}

async fn history(State(state): State<AppState>, user: CurrentUser, Query(p): Query<HistoryParams>) -> Reply {
    authorize(&user)?;
    let page = activity(&state)?
        .history(&HistoryQuery {
            page: p.page,
            page_size: p.page_size,
            movie_id: p.movie_id,
            event_type: p.event_type.as_deref().and_then(super::history_event_from_arr),
            ..Default::default()
        })
        .await?;
    let records = page
        .items
        .iter()
        .filter(|h| h.media.as_ref().is_some_and(|m| m.kind == "movie"))
        .map(|h| history_record(App::Radarr, h))
        .collect();
    let q = PageQuery {
        page: p.page,
        page_size: p.page_size,
        ..Default::default()
    };
    ok(paged(&q, page.total, records))
}

async fn movie_history(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(p): Query<HistoryParams>,
) -> Reply {
    authorize(&user)?;
    let id = p
        .movie_id
        .ok_or_else(|| ApiError::validation("movieId is required"))?;
    let page = activity(&state)?
        .history(&HistoryQuery {
            movie_id: Some(id),
            page_size: Some(500),
            ..Default::default()
        })
        .await?;
    ok(Value::Array(
        page.items
            .iter()
            .map(|h| history_record(App::Radarr, h))
            .collect(),
    ))
}

pub(super) async fn command(state: &AppState, b: &CommandBody) -> ApiResult<Option<i64>> {
    let lib = library(state)?;
    let mut ids = b.movie_ids.clone();
    ids.extend(b.movie_id);
    match b.name.as_str() {
        "MoviesSearch" | "MovieSearch" => Ok(lib
            .request_search(SearchRequest {
                kind: LibrarySearchKind::Movie,
                ids,
                season: None,
                media: None,
            })
            .await?),
        "MissingMoviesSearch" => Ok(lib
            .request_search(SearchRequest {
                kind: LibrarySearchKind::Missing,
                ids: vec![],
                season: None,
                media: Some(CalendarKind::Movie),
            })
            .await?),
        "CutOffUnmetMoviesSearch" => Ok(lib
            .request_search(SearchRequest {
                kind: LibrarySearchKind::Cutoff,
                ids: vec![],
                season: None,
                media: Some(CalendarKind::Movie),
            })
            .await?),
        "RefreshMovie" => {
            let mut last = None;
            if ids.is_empty() {
                return run_task(state, reelix_library::kinds::REFRESH_MOVIES).await;
            }
            for id in ids {
                last = lib.queue_refresh_movie(id).await?.or(last);
            }
            Ok(last)
        }
        "RescanMovie" => Ok(lib.queue_scan(&ids, &[]).await?),
        "RssSync" => run_task(state, "rss_sync").await,
        "DownloadedMoviesScan" | "ProcessMonitoredDownloads" => run_task(state, "download_monitor").await,
        other => Err(unknown_command(other)),
    }
}
