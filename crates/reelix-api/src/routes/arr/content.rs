//! Readarr v1 and Lidarr v1: the read side (authors/books, artists/albums, wanted) plus
//! searches, enough for dashboards and mobile apps.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use reelix_api_types::books::{
    AuthorDto, AuthorListQuery, BookDto, BookListQuery, BookSearchRequest, BookWantedQuery,
};
use reelix_api_types::content::ContentSearchKind;
use reelix_api_types::music::{
    AlbumDto, AlbumListQuery, AlbumSearchRequest, AlbumWantedQuery, ArtistDto, ArtistListQuery,
};
use reelix_books::BooksService;
use reelix_music::MusicService;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    App, CommandBody, PageQuery, Reply, authorize, date, ok, paged, r_get, run_task, unknown_command,
};
use crate::auth::CurrentUser;
use crate::error::ApiResult;
use crate::state::AppState;

fn books(state: &AppState) -> ApiResult<Arc<BooksService>> {
    state.ext::<BooksService>()
}

fn music(state: &AppState) -> ApiResult<Arc<MusicService>> {
    state.ext::<MusicService>()
}

pub(super) fn readarr_routes() -> Router<AppState> {
    Router::new()
        .route("/author", r_get(authors))
        .route("/author/{id}", r_get(author))
        .route("/book", r_get(book_list))
        .route("/wanted/missing", r_get(wanted_books))
        .route("/wanted/cutoff", r_get(cutoff_books))
        .route("/calendar", r_get(empty_list))
        .route("/metadataprofile", r_get(book_metadata_profiles))
}

pub(super) fn lidarr_routes() -> Router<AppState> {
    Router::new()
        .route("/artist", r_get(artists))
        .route("/artist/{id}", r_get(artist))
        .route("/album", r_get(album_list))
        .route("/wanted/missing", r_get(wanted_albums))
        .route("/wanted/cutoff", r_get(cutoff_albums))
        .route("/calendar", r_get(empty_list))
        .route("/metadataprofile", r_get(music_metadata_profiles))
}

async fn empty_list(user: CurrentUser) -> Reply {
    authorize(&user)?;
    ok(json!([]))
}

// ---------------------------------------------------------------- Readarr

fn author_json(a: &AuthorDto) -> Value {
    let s = &a.statistics;
    json!({
        "id": a.id,
        "authorName": a.name,
        "sortName": a.sort_name,
        "foreignAuthorId": a.openlibrary_id,
        "titleSlug": a.openlibrary_id,
        "monitored": a.monitored,
        "path": a.path.clone().or_else(|| a.audio_path.clone()).unwrap_or_default(),
        "qualityProfileId": a.quality_profile_id.or(a.audio_quality_profile_id).unwrap_or(0),
        "metadataProfileId": 1,
        "tags": a.tags,
        "added": a.added,
        "status": "continuing",
        "statistics": {
            "bookCount": s.book_count,
            "bookFileCount": s.ebook_file_count + s.audiobook_file_count,
            "totalBookCount": s.book_count,
            "sizeOnDisk": s.size_on_disk,
            "percentOfBooks": if s.book_count == 0 { 0.0 } else {
                f64::from(s.ebook_file_count.max(s.audiobook_file_count)) * 100.0 / f64::from(s.book_count)
            },
        },
    })
}

fn book_json(b: &BookDto) -> Value {
    let files = u32::from(b.ebook_file.is_some()) + u32::from(b.audiobook_file.is_some());
    let size = b.ebook_file.as_ref().map_or(0, |f| f.size) + b.audiobook_file.as_ref().map_or(0, |f| f.size);
    json!({
        "id": b.id,
        "authorId": b.author_id,
        "author": {"id": b.author_id, "authorName": b.author_name},
        "title": b.title,
        "foreignBookId": b.openlibrary_work_id,
        "titleSlug": b.openlibrary_work_id,
        "releaseDate": date(b.release_date),
        "monitored": b.monitored_ebook || b.monitored_audiobook,
        "pageCount": b.page_count.unwrap_or(0),
        "isbn13": b.isbn13,
        "asin": b.asin,
        "added": b.added,
        "grabbed": false,
        "statistics": {
            "bookFileCount": files,
            "bookCount": 1,
            "totalBookCount": 1,
            "sizeOnDisk": size,
            "percentOfBooks": if files > 0 { 100.0 } else { 0.0 },
        },
    })
}

async fn all_authors(state: &AppState) -> ApiResult<Vec<AuthorDto>> {
    Ok(books(state)?
        .list_authors(&AuthorListQuery::default())
        .await?
        .items)
}

async fn authors(State(state): State<AppState>, user: CurrentUser) -> Reply {
    authorize(&user)?;
    ok(Value::Array(
        all_authors(&state).await?.iter().map(author_json).collect(),
    ))
}

async fn author(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    ok(author_json(&books(&state)?.get_author(id).await?))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookQuery {
    author_id: Option<i64>,
}

async fn book_list(State(state): State<AppState>, user: CurrentUser, Query(q): Query<BookQuery>) -> Reply {
    authorize(&user)?;
    let list = books(&state)?
        .list_books(&BookListQuery {
            author_id: q.author_id,
            ..Default::default()
        })
        .await?;
    ok(Value::Array(list.items.iter().map(book_json).collect()))
}

async fn book_wanted(state: &AppState, q: &PageQuery, kind: &str) -> Reply {
    let page = books(state)?
        .wanted(&BookWantedQuery {
            kind: Some(kind.into()),
            format: None,
            page: q.page,
            page_size: Some(q.page_size.unwrap_or(10)),
        })
        .await?;
    ok(paged(q, page.total, page.items.iter().map(book_json).collect()))
}

async fn wanted_books(State(state): State<AppState>, user: CurrentUser, Query(q): Query<PageQuery>) -> Reply {
    authorize(&user)?;
    book_wanted(&state, &q, "missing").await
}

async fn cutoff_books(State(state): State<AppState>, user: CurrentUser, Query(q): Query<PageQuery>) -> Reply {
    authorize(&user)?;
    book_wanted(&state, &q, "cutoff").await
}

async fn book_metadata_profiles(user: CurrentUser) -> Reply {
    authorize(&user)?;
    ok(json!([{"id": 1, "name": "Standard"}]))
}

// ---------------------------------------------------------------- Lidarr

fn artist_json(a: &ArtistDto) -> Value {
    let s = &a.statistics;
    json!({
        "id": a.id,
        "artistName": a.name,
        "sortName": a.sort_name,
        "foreignArtistId": a.musicbrainz_id,
        "mbId": a.musicbrainz_id,
        "monitored": a.monitored,
        "path": a.path,
        "qualityProfileId": a.quality_profile_id,
        "metadataProfileId": a.metadata_profile_id,
        "tags": a.tags,
        "status": "continuing",
        "statistics": {
            "albumCount": s.album_count,
            "trackFileCount": s.track_file_count,
            "trackCount": s.track_count,
            "totalTrackCount": s.track_count,
            "sizeOnDisk": s.size_on_disk,
            "percentOfTracks": if s.track_count == 0 { 0.0 } else {
                f64::from(s.track_file_count) * 100.0 / f64::from(s.track_count)
            },
        },
    })
}

fn album_json(a: &AlbumDto) -> Value {
    let s = &a.statistics;
    json!({
        "id": a.id,
        "artistId": a.artist_id,
        "artist": {"id": a.artist_id, "artistName": a.artist_name},
        "title": a.title,
        "foreignAlbumId": a.musicbrainz_release_group_id,
        "albumType": a.album_type,
        "releaseDate": date(a.release_date),
        "monitored": a.monitored,
        "statistics": {
            "trackFileCount": s.track_file_count,
            "trackCount": s.track_count,
            "totalTrackCount": s.track_count,
            "sizeOnDisk": s.size_on_disk,
            "percentOfTracks": if s.track_count == 0 { 0.0 } else {
                f64::from(s.track_file_count) * 100.0 / f64::from(s.track_count)
            },
        },
    })
}

async fn artists(State(state): State<AppState>, user: CurrentUser) -> Reply {
    authorize(&user)?;
    let list = music(&state)?.list_artists(&ArtistListQuery::default()).await?;
    ok(Value::Array(list.items.iter().map(artist_json).collect()))
}

async fn artist(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    ok(artist_json(&music(&state)?.get_artist(id).await?))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumQuery {
    artist_id: Option<i64>,
}

async fn album_list(State(state): State<AppState>, user: CurrentUser, Query(q): Query<AlbumQuery>) -> Reply {
    authorize(&user)?;
    let list = music(&state)?
        .list_albums(&AlbumListQuery {
            artist_id: q.artist_id,
            ..Default::default()
        })
        .await?;
    ok(Value::Array(list.items.iter().map(album_json).collect()))
}

async fn album_wanted(state: &AppState, q: &PageQuery, kind: &str) -> Reply {
    let page = music(state)?
        .wanted(&AlbumWantedQuery {
            kind: Some(kind.into()),
            page: q.page,
            page_size: Some(q.page_size.unwrap_or(10)),
        })
        .await?;
    ok(paged(q, page.total, page.items.iter().map(album_json).collect()))
}

async fn wanted_albums(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<PageQuery>,
) -> Reply {
    authorize(&user)?;
    album_wanted(&state, &q, "missing").await
}

async fn cutoff_albums(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<PageQuery>,
) -> Reply {
    authorize(&user)?;
    album_wanted(&state, &q, "cutoff").await
}

async fn music_metadata_profiles(State(state): State<AppState>, user: CurrentUser) -> Reply {
    authorize(&user)?;
    let list = music(&state)?.metadata_profiles().await?;
    ok(Value::Array(
        list.iter().map(|p| json!({"id": p.id, "name": p.name})).collect(),
    ))
}

// ---------------------------------------------------------------- commands

pub(super) async fn command(state: &AppState, app: App, b: &CommandBody) -> ApiResult<Option<i64>> {
    match (app, b.name.as_str()) {
        (App::Readarr, "BookSearch") => Ok(books(state)?
            .request_search(BookSearchRequest {
                kind: ContentSearchKind::Item,
                ids: b.book_ids.clone(),
                format: None,
            })
            .await?),
        (App::Readarr, "AuthorSearch") => Ok(books(state)?
            .request_search(BookSearchRequest {
                kind: ContentSearchKind::Parent,
                ids: b.author_id.into_iter().collect(),
                format: None,
            })
            .await?),
        (App::Readarr, "MissingBookSearch") => Ok(books(state)?
            .request_search(BookSearchRequest {
                kind: ContentSearchKind::Missing,
                ids: vec![],
                format: None,
            })
            .await?),
        (App::Readarr, "RefreshAuthor") => run_task(state, reelix_books::kinds::REFRESH_AUTHORS).await,
        (App::Lidarr, "AlbumSearch") => Ok(music(state)?
            .request_search(AlbumSearchRequest {
                kind: ContentSearchKind::Item,
                ids: b.album_ids.clone(),
            })
            .await?),
        (App::Lidarr, "ArtistSearch") => Ok(music(state)?
            .request_search(AlbumSearchRequest {
                kind: ContentSearchKind::Parent,
                ids: b.artist_id.into_iter().collect(),
            })
            .await?),
        (App::Lidarr, "MissingAlbumSearch") => Ok(music(state)?
            .request_search(AlbumSearchRequest {
                kind: ContentSearchKind::Missing,
                ids: vec![],
            })
            .await?),
        (App::Lidarr, "RefreshArtist") => run_task(state, reelix_music::kinds::REFRESH_ARTISTS).await,
        (_, "RssSync") => run_task(state, "rss_sync").await,
        (_, other) => Err(unknown_command(other)),
    }
}
