//! Music (M9): artists, albums, releases, tracks, track files, metadata profiles, music root
//! folders, library import, renaming and music settings.
//!
//! Reads and writes need `MANAGE_LIBRARY`; music settings, root-folder changes and
//! metadata-profile changes need `ADMIN`. DTOs: `reelix_api_types::music`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use reelix_api_types::content::{ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_api_types::library::{
    DeleteQuery, LibraryImportMatchRequest, LibraryImportResult, LookupQuery, Paged, RenamePreviewDto,
};
use reelix_api_types::music::{
    AddAlbumRequest, AddArtistRequest, AlbumDto, AlbumEdit, AlbumIdQuery, AlbumListQuery, AlbumLookupDto,
    AlbumMonitorRequest, AlbumReleaseDto, AlbumSearchRequest, AlbumWantedQuery, ArtistDto, ArtistEdit,
    ArtistJobRequest, ArtistListQuery, ArtistLookupDto, ChooseReleaseRequest, MetadataProfileDto,
    MusicImportCandidateDto, MusicImportScanQuery, MusicLibraryImportRequest, MusicRenameQuery,
    MusicSettingsDto, TrackDto, TrackFileDto,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob};
use reelix_core::Permissions;
use reelix_music::{MusicError, MusicService};
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::routes::library::{accepted, created, require_lib};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/artists", get(list_artists).post(add_artist))
        .route("/artists/lookup", get(lookup_artists))
        .route("/artists/refresh", post(refresh_artists))
        .route("/artists/rescan", post(rescan_artists))
        .route(
            "/artists/{id}",
            get(get_artist).put(edit_artist).delete(delete_artist),
        )
        .route("/artists/{id}/refresh", post(refresh_artist))
        .route("/artists/{id}/rescan", post(rescan_artist))
        .route("/albums", get(list_albums).post(add_album))
        .route("/albums/lookup", get(lookup_albums))
        .route("/albums/wanted", get(wanted_albums))
        .route("/albums/monitor", put(monitor_albums))
        .route(
            "/albums/{id}",
            get(get_album).put(edit_album).delete(delete_album),
        )
        .route("/albums/{id}/releases", get(album_releases))
        .route("/albums/{id}/release", put(choose_release))
        .route("/albums/{id}/search", post(search_album))
        .route("/music/search", post(music_search))
        .route("/music/rename", get(rename_preview).post(rename_run))
        .route("/tracks", get(list_tracks))
        .route("/trackfiles", get(list_track_files))
        .route("/trackfiles/{id}", delete(delete_track_file))
        .route(
            "/metadataprofiles",
            get(list_metadata_profiles).post(create_metadata_profile),
        )
        .route(
            "/metadataprofiles/{id}",
            get(get_metadata_profile)
                .put(update_metadata_profile)
                .delete(delete_metadata_profile),
        )
        .route("/musicrootfolders", get(list_root_folders).post(add_root_folder))
        .route("/musicrootfolders/{id}", delete(delete_root_folder))
        .route("/musicimport/scan", get(import_scan))
        .route("/musicimport/match", post(import_match))
        .route("/musicimport", post(library_import))
        .route("/settings/music", get(get_settings).put(put_settings))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_artists, add_artist, lookup_artists, refresh_artists, rescan_artists, get_artist, edit_artist,
        delete_artist, refresh_artist, rescan_artist,
        list_albums, add_album, lookup_albums, wanted_albums, monitor_albums, get_album, edit_album, delete_album,
        album_releases, choose_release, search_album, music_search, rename_preview, rename_run,
        list_tracks, list_track_files, delete_track_file,
        list_metadata_profiles, create_metadata_profile, get_metadata_profile, update_metadata_profile,
        delete_metadata_profile, list_root_folders, add_root_folder, delete_root_folder,
        import_scan, import_match, library_import, get_settings, put_settings,
    ),
    components(schemas(
        reelix_api_types::music::AlbumType,
        reelix_api_types::music::SecondaryAlbumType,
        reelix_api_types::music::ReleaseStatus,
        reelix_api_types::music::NewAlbumMonitor,
        reelix_api_types::music::ArtistMonitor,
        reelix_api_types::music::MetadataProfileDto,
        reelix_api_types::music::ArtistStatisticsDto,
        reelix_api_types::music::ArtistDto,
        reelix_api_types::music::AlbumReleaseDto,
        reelix_api_types::music::AlbumStatisticsDto,
        reelix_api_types::music::AlbumDto,
        reelix_api_types::music::TrackFileDto,
        reelix_api_types::music::TrackDto,
        reelix_api_types::music::ArtistLookupDto,
        reelix_api_types::music::AlbumLookupDto,
        reelix_api_types::music::AddArtistRequest,
        reelix_api_types::music::AddAlbumRequest,
        reelix_api_types::music::ArtistEdit,
        reelix_api_types::music::AlbumEdit,
        reelix_api_types::music::AlbumMonitorRequest,
        reelix_api_types::music::ChooseReleaseRequest,
        reelix_api_types::music::AlbumSearchRequest,
        reelix_api_types::music::ArtistJobRequest,
        reelix_api_types::music::MusicNamingDto,
        reelix_api_types::music::MusicSettingsDto,
        reelix_api_types::music::ReleaseMatchDto,
        reelix_api_types::music::MusicImportCandidateDto,
        reelix_api_types::music::MusicImportItem,
        reelix_api_types::music::MusicLibraryImportRequest,
        reelix_api_types::content::ContentRootFolderDto,
        reelix_api_types::content::CreateContentRootFolderRequest,
        reelix_api_types::content::ContentSearchKind,
    )),
    tags((name = "music", description = "Artists, albums, releases, tracks, track files, metadata profiles, music root folders, library import, renaming and music settings")),
)]
pub struct Api;

pub(crate) fn svc(state: &AppState) -> ApiResult<Arc<MusicService>> {
    state.ext::<MusicService>()
}

impl From<MusicError> for ApiError {
    fn from(e: MusicError) -> Self {
        match e {
            MusicError::NotFound(m) => ApiError::not_found(m),
            MusicError::Validation(m) => ApiError::validation(m),
            MusicError::Conflict(m) => ApiError::Conflict(m),
            MusicError::Metadata(meta) => {
                let msg = meta.to_string();
                if msg == "not found" {
                    ApiError::not_found("not found on MusicBrainz")
                } else if msg.contains("not configured") || msg.contains("unsupported") {
                    ApiError::Validation {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        message: format!("metadata provider: {msg}"),
                    }
                } else {
                    ApiError::Validation {
                        status: StatusCode::BAD_GATEWAY,
                        message: format!("metadata provider: {msg}"),
                    }
                }
            }
            MusicError::Files(f) => ApiError::validation(f.to_string()),
            MusicError::NotImplemented(what) => ApiError::Validation {
                status: StatusCode::NOT_IMPLEMENTED,
                message: format!("{what} is not implemented yet"),
            },
            other => ApiError::internal(other),
        }
    }
}

fn require_admin(user: &CurrentUser) -> ApiResult<()> {
    user.require(Permissions::ADMIN)
}

// ---------------- artists ----------------

/// Artists (filterable, sortable, optionally paged).
#[utoipa::path(get, path = "/api/v1/artists", tag = "music", params(ArtistListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Artists", body = Paged<ArtistDto>),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
    ))]
pub async fn list_artists(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<ArtistListQuery>,
) -> ApiResult<ApiJson<Paged<ArtistDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.list_artists(&q).await?))
}

/// Adds an artist from MusicBrainz with its albums (filtered by the metadata profile).
#[utoipa::path(post, path = "/api/v1/artists", tag = "music", request_body = AddArtistRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Added", body = ArtistDto),
        (status = 400, description = "Invalid profile/root folder", body = ApiErrorBody),
        (status = 409, description = "Already in the library", body = ApiErrorBody),
        (status = 502, description = "MusicBrainz failed", body = ApiErrorBody),
    ))]
pub async fn add_artist(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AddArtistRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(created(svc(&state)?.add_artist(req).await?))
}

/// MusicBrainz artist search with in-library ids (`mbid:<uuid>` supported).
#[utoipa::path(get, path = "/api/v1/artists/lookup", tag = "music", params(LookupQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Results", body = Vec<ArtistLookupDto>)))]
pub async fn lookup_artists(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<LookupQuery>,
) -> ApiResult<ApiJson<Vec<ArtistLookupDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.lookup_artists(&q.term).await?))
}

/// Queues a metadata refresh of the given artists (empty = all).
#[utoipa::path(post, path = "/api/v1/artists/refresh", tag = "music", request_body = ArtistJobRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob)))]
pub async fn refresh_artists(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<ArtistJobRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_refresh(req.artist_ids).await?))
}

/// Queues a disk scan of the given artists (empty = all).
#[utoipa::path(post, path = "/api/v1/artists/rescan", tag = "music", request_body = ArtistJobRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob)))]
pub async fn rescan_artists(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<ArtistJobRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_rescan(req.artist_ids).await?))
}

/// One artist.
#[utoipa::path(get, path = "/api/v1/artists/{id}", tag = "music",
    params(("id" = i64, Path, description = "Artist id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Artist", body = ArtistDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_artist(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<ArtistDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_artist(id).await?))
}

/// Edits an artist (partial; `move_files` moves the folder when `path` changes).
#[utoipa::path(put, path = "/api/v1/artists/{id}", tag = "music", request_body = ArtistEdit,
    params(("id" = i64, Path, description = "Artist id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = ArtistDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn edit_artist(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(edit): ApiJson<ArtistEdit>,
) -> ApiResult<ApiJson<ArtistDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.update_artist(id, edit).await?))
}

/// Deletes an artist (`?delete_files=true` also removes its folder).
#[utoipa::path(delete, path = "/api/v1/artists/{id}", tag = "music",
    params(("id" = i64, Path, description = "Artist id"), DeleteQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_artist(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<DeleteQuery>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_artist(id, q.delete_files).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Queues a metadata refresh of one artist.
#[utoipa::path(post, path = "/api/v1/artists/{id}/refresh", tag = "music",
    params(("id" = i64, Path, description = "Artist id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn refresh_artist(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let s = svc(&state)?;
    s.get_artist(id).await?;
    Ok(accepted(s.queue_refresh(vec![id]).await?))
}

/// Queues a disk scan of one artist.
#[utoipa::path(post, path = "/api/v1/artists/{id}/rescan", tag = "music",
    params(("id" = i64, Path, description = "Artist id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn rescan_artist(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let s = svc(&state)?;
    s.get_artist(id).await?;
    Ok(accepted(s.queue_rescan(vec![id]).await?))
}

// ---------------- albums ----------------

/// Albums (by artist, search, monitored, missing; optionally paged).
#[utoipa::path(get, path = "/api/v1/albums", tag = "music", params(AlbumListQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Albums", body = Paged<AlbumDto>)))]
pub async fn list_albums(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<AlbumListQuery>,
) -> ApiResult<ApiJson<Paged<AlbumDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.list_albums(&q).await?))
}

/// Adds one album (creating its artist, with nothing else monitored, when missing).
#[utoipa::path(post, path = "/api/v1/albums", tag = "music", request_body = AddAlbumRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Added", body = AlbumDto),
        (status = 409, description = "Already in the library", body = ApiErrorBody),
    ))]
pub async fn add_album(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AddAlbumRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(created(svc(&state)?.add_album(req).await?))
}

/// MusicBrainz release-group search with in-library ids (`mbid:<uuid>` supported).
#[utoipa::path(get, path = "/api/v1/albums/lookup", tag = "music", params(LookupQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Results", body = Vec<AlbumLookupDto>)))]
pub async fn lookup_albums(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<LookupQuery>,
) -> ApiResult<ApiJson<Vec<AlbumLookupDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.lookup_albums(&q.term).await?))
}

/// Wanted albums: `kind=missing` (default) or `kind=cutoff`.
#[utoipa::path(get, path = "/api/v1/albums/wanted", tag = "music", params(AlbumWantedQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Albums", body = Paged<AlbumDto>)))]
pub async fn wanted_albums(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<AlbumWantedQuery>,
) -> ApiResult<ApiJson<Paged<AlbumDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.wanted(&q).await?))
}

/// Monitors or unmonitors several albums.
#[utoipa::path(put, path = "/api/v1/albums/monitor", tag = "music", request_body = AlbumMonitorRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated albums", body = Vec<AlbumDto>)))]
pub async fn monitor_albums(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AlbumMonitorRequest>,
) -> ApiResult<ApiJson<Vec<AlbumDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.monitor_albums(req).await?))
}

/// One album with its releases.
#[utoipa::path(get, path = "/api/v1/albums/{id}", tag = "music",
    params(("id" = i64, Path, description = "Album id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Album", body = AlbumDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_album(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<AlbumDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_album(id).await?))
}

/// Edits an album (monitored, any release accepted at import).
#[utoipa::path(put, path = "/api/v1/albums/{id}", tag = "music", request_body = AlbumEdit,
    params(("id" = i64, Path, description = "Album id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = AlbumDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn edit_album(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(edit): ApiJson<AlbumEdit>,
) -> ApiResult<ApiJson<AlbumDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.update_album(id, edit).await?))
}

/// Deletes an album (`?delete_files=true` also removes its track files).
#[utoipa::path(delete, path = "/api/v1/albums/{id}", tag = "music",
    params(("id" = i64, Path, description = "Album id"), DeleteQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_album(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<DeleteQuery>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_album(id, q.delete_files).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Releases (editions) of an album; `selected` marks the wanted one.
#[utoipa::path(get, path = "/api/v1/albums/{id}/releases", tag = "music",
    params(("id" = i64, Path, description = "Album id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Releases", body = Vec<AlbumReleaseDto>), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn album_releases(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<Vec<AlbumReleaseDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_album(id).await?.releases))
}

/// Chooses the album's release (its tracklist is what gets searched and imported).
#[utoipa::path(put, path = "/api/v1/albums/{id}/release", tag = "music", request_body = ChooseReleaseRequest,
    params(("id" = i64, Path, description = "Album id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = AlbumDto),
        (status = 400, description = "Release of another album", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn choose_release(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<ChooseReleaseRequest>,
) -> ApiResult<ApiJson<AlbumDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.choose_release(id, req).await?))
}

/// Queues a search for one album (job `album_search`).
#[utoipa::path(post, path = "/api/v1/albums/{id}/search", tag = "music",
    params(("id" = i64, Path, description = "Album id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn search_album(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let s = svc(&state)?;
    s.get_album(id).await?;
    Ok(accepted(
        s.request_search(AlbumSearchRequest {
            kind: reelix_api_types::content::ContentSearchKind::Item,
            ids: vec![id],
        })
        .await?,
    ))
}

/// Queues an album search (albums, artists, all missing or all below cutoff).
#[utoipa::path(post, path = "/api/v1/music/search", tag = "music", request_body = AlbumSearchRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 400, description = "Missing ids", body = ApiErrorBody)))]
pub async fn music_search(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AlbumSearchRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.request_search(req).await?))
}

/// Files of an artist whose names differ from the naming settings.
#[utoipa::path(get, path = "/api/v1/music/rename", tag = "music", params(MusicRenameQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Planned renames (season = disc, episodes = tracks)", body = Vec<RenamePreviewDto>)))]
pub async fn rename_preview(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<MusicRenameQuery>,
) -> ApiResult<ApiJson<Vec<RenamePreviewDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.rename_preview(q.artist_id).await?))
}

/// Queues the renames of an artist (job `music_rename`).
#[utoipa::path(post, path = "/api/v1/music/rename", tag = "music", params(MusicRenameQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn rename_run(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<MusicRenameQuery>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_rename(q.artist_id).await?))
}

// ---------------- tracks / files ----------------

/// Tracks of the album's selected release, with their files.
#[utoipa::path(get, path = "/api/v1/tracks", tag = "music", params(AlbumIdQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Tracks", body = Vec<TrackDto>), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn list_tracks(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<AlbumIdQuery>,
) -> ApiResult<ApiJson<Vec<TrackDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.tracks(q.album_id).await?))
}

/// Track files of an album.
#[utoipa::path(get, path = "/api/v1/trackfiles", tag = "music", params(AlbumIdQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Track files", body = Vec<TrackFileDto>), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn list_track_files(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<AlbumIdQuery>,
) -> ApiResult<ApiJson<Vec<TrackFileDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.track_files(q.album_id).await?))
}

/// Deletes a track file (to the recycle bin when configured).
#[utoipa::path(delete, path = "/api/v1/trackfiles/{id}", tag = "music",
    params(("id" = i64, Path, description = "Track file id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_track_file(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_track_file(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- metadata profiles ----------------

/// Metadata profiles (which album types an artist gets).
#[utoipa::path(get, path = "/api/v1/metadataprofiles", tag = "music",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Profiles", body = Vec<MetadataProfileDto>)))]
pub async fn list_metadata_profiles(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<MetadataProfileDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.metadata_profiles().await?))
}

/// Creates a metadata profile.
#[utoipa::path(post, path = "/api/v1/metadataprofiles", tag = "music", request_body = MetadataProfileDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = MetadataProfileDto),
        (status = 400, description = "Invalid", body = ApiErrorBody),
        (status = 409, description = "Name taken", body = ApiErrorBody),
    ))]
pub async fn create_metadata_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(p): ApiJson<MetadataProfileDto>,
) -> ApiResult<Response> {
    require_admin(&user)?;
    Ok(created(svc(&state)?.create_metadata_profile(p).await?))
}

/// One metadata profile.
#[utoipa::path(get, path = "/api/v1/metadataprofiles/{id}", tag = "music",
    params(("id" = i64, Path, description = "Profile id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Profile", body = MetadataProfileDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_metadata_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<MetadataProfileDto>> {
    require_lib(&user)?;
    svc(&state)?
        .metadata_profiles()
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .map(ApiJson)
        .ok_or_else(|| ApiError::not_found(format!("metadata profile {id} not found")))
}

/// Replaces a metadata profile.
#[utoipa::path(put, path = "/api/v1/metadataprofiles/{id}", tag = "music", request_body = MetadataProfileDto,
    params(("id" = i64, Path, description = "Profile id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = MetadataProfileDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn update_metadata_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(p): ApiJson<MetadataProfileDto>,
) -> ApiResult<ApiJson<MetadataProfileDto>> {
    require_admin(&user)?;
    Ok(ApiJson(svc(&state)?.update_metadata_profile(id, p).await?))
}

/// Deletes a metadata profile (refused while artists use it).
#[utoipa::path(delete, path = "/api/v1/metadataprofiles/{id}", tag = "music",
    params(("id" = i64, Path, description = "Profile id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 409, description = "In use", body = ApiErrorBody)))]
pub async fn delete_metadata_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    svc(&state)?.delete_metadata_profile(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- root folders / import / settings ----------------

/// Music root folders with free space and unmapped folders.
#[utoipa::path(get, path = "/api/v1/musicrootfolders", tag = "music",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Root folders", body = Vec<ContentRootFolderDto>)))]
pub async fn list_root_folders(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<ContentRootFolderDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.root_folders().await?))
}

/// Adds a music root folder (kind `music`; must exist; may not overlap any root folder).
#[utoipa::path(post, path = "/api/v1/musicrootfolders", tag = "music", request_body = CreateContentRootFolderRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = ContentRootFolderDto),
        (status = 400, description = "Missing/relative path or wrong kind", body = ApiErrorBody),
        (status = 409, description = "Duplicate or overlapping", body = ApiErrorBody),
    ))]
pub async fn add_root_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<CreateContentRootFolderRequest>,
) -> ApiResult<Response> {
    require_admin(&user)?;
    Ok(created(svc(&state)?.add_root_folder(req).await?))
}

/// Removes a music root folder (refused while artists use it).
#[utoipa::path(delete, path = "/api/v1/musicrootfolders/{id}", tag = "music",
    params(("id" = i64, Path, description = "Root folder id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 409, description = "In use", body = ApiErrorBody)))]
pub async fn delete_root_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    svc(&state)?.delete_root_folder(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Folders under a music root that are not artists yet, with MusicBrainz suggestions (for the
/// first 25 folders; `lookup=false` lists folders only).
#[utoipa::path(get, path = "/api/v1/musicimport/scan", tag = "music", params(MusicImportScanQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Candidates", body = Vec<MusicImportCandidateDto>)))]
pub async fn import_scan(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<MusicImportScanQuery>,
) -> ApiResult<ApiJson<Vec<MusicImportCandidateDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(
        svc(&state)?
            .import_scan(q.root_folder_id, q.lookup.unwrap_or(true))
            .await?,
    ))
}

/// MusicBrainz suggestions for up to 25 folders from a scan (`lookup=false`), so every folder of
/// a large library is matched in short requests.
#[utoipa::path(post, path = "/api/v1/musicimport/match", tag = "music", request_body = LibraryImportMatchRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Candidates with suggestions", body = Vec<MusicImportCandidateDto>),
        (status = 400, description = "Too many folders or a folder outside the root folder", body = ApiErrorBody),
    ))]
pub async fn import_match(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<LibraryImportMatchRequest>,
) -> ApiResult<ApiJson<Vec<MusicImportCandidateDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(
        svc(&state)?.import_match(req.root_folder_id, req.paths).await?,
    ))
}

/// Adds the chosen folders as artists and scans their files.
#[utoipa::path(post, path = "/api/v1/musicimport", tag = "music", request_body = MusicLibraryImportRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Added artists and per-folder errors", body = LibraryImportResult)))]
pub async fn library_import(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<MusicLibraryImportRequest>,
) -> ApiResult<ApiJson<LibraryImportResult>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.library_import(req).await?))
}

/// Music settings (naming, tags/covers, release preferences).
#[utoipa::path(get, path = "/api/v1/settings/music", tag = "music",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Settings", body = MusicSettingsDto), (status = 403, description = "Requires ADMIN", body = ApiErrorBody)))]
pub async fn get_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<MusicSettingsDto>> {
    require_admin(&user)?;
    Ok(ApiJson(svc(&state)?.settings().await?))
}

/// Validates and stores the music settings.
#[utoipa::path(put, path = "/api/v1/settings/music", tag = "music", request_body = MusicSettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Saved", body = MusicSettingsDto), (status = 400, description = "Invalid template", body = ApiErrorBody)))]
pub async fn put_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<MusicSettingsDto>,
) -> ApiResult<ApiJson<MusicSettingsDto>> {
    require_admin(&user)?;
    Ok(ApiJson(svc(&state)?.set_settings(s).await?))
}
