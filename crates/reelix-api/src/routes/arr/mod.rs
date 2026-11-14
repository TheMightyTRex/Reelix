//! Radarr v3, Sonarr v3, Readarr v1 and Lidarr v1 compatible APIs, so tools written for the
//! *arr apps (Homepage, Bazarr, LunaSea, nzb360, Recyclarr, …) work with Reelix.
//!
//! Each app gets its own prefix because their APIs overlap (`/api/v3/system/status` exists in
//! both Radarr and Sonarr): point a Radarr client at `http://reelix:7700/radarr`, a Sonarr
//! client at `/sonarr`, and so on. Authentication is the usual `X-Api-Key` header or `apikey`
//! query with a Reelix API key; its user needs the Manage library permission.
//!
//! Only the subset those tools use is implemented, in the *arr JSON shapes (camelCase, paged
//! `records` envelopes). Ids are Reelix ids.

mod content;
mod radarr;
mod signalr;
mod sonarr;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, put};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use reelix_api_types::library::{MediaFileDto, MediaInfoDto};
use reelix_api_types::pipeline::{QueueItemDto, QueueRemoveQuery};
use reelix_api_types::{HealthCheck, HealthLevel};
use reelix_core::{ContentKind, MediaKind, Permissions};
use reelix_decision::{ProfileItem, QualityProfile};
use reelix_library::LibraryService;
use reelix_parser::{Quality, QualityFamily, QualityModel};
use reelix_pipeline::ActivityService;
use reelix_providers::ProviderService;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Which app an endpoint is pretending to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum App {
    Radarr,
    Sonarr,
    Readarr,
    Lidarr,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Radarr => "Radarr",
            App::Sonarr => "Sonarr",
            App::Readarr => "Readarr",
            App::Lidarr => "Lidarr",
        }
    }

    /// A version recent enough that clients pick their current API code paths.
    fn version(self) -> &'static str {
        match self {
            App::Radarr => "5.14.0.9383",
            App::Sonarr => "4.0.11.2680",
            App::Readarr => "0.4.4.2686",
            App::Lidarr => "2.8.2.4493",
        }
    }

    fn family(self) -> QualityFamily {
        match self {
            App::Radarr | App::Sonarr => QualityFamily::Video,
            App::Readarr => QualityFamily::Ebook,
            App::Lidarr => QualityFamily::Music,
        }
    }

    fn root_kinds(self) -> &'static [ContentKind] {
        match self {
            App::Radarr => &[ContentKind::Movie],
            App::Sonarr => &[ContentKind::Series],
            App::Readarr => &[ContentKind::Ebook, ContentKind::Audiobook],
            App::Lidarr => &[ContentKind::Music],
        }
    }
}

/// `/radarr/api/v3/…`, `/sonarr/api/v3/…`, `/readarr/api/v1/…`, `/lidarr/api/v1/…`.
pub fn routes() -> axum::Router<AppState> {
    let common = || {
        Router::new()
            .route("/system/status", get(system_status))
            .route("/health", get(health))
            .route("/diskspace", get(diskspace))
            .route("/tag", get(tags).post(create_tag))
            .route("/tag/{id}", get(tag))
            .route("/rootfolder", get(root_folders))
            .route(
                "/qualityprofile",
                get(quality_profiles).post(create_quality_profile),
            )
            .route(
                "/qualityprofile/{id}",
                get(quality_profile).put(update_quality_profile),
            )
            .route("/qualitydefinition", get(quality_definitions))
            .route("/qualitydefinition/update", put(update_quality_definitions))
            .route("/queue", get(queue))
            .route("/queue/status", get(queue_status))
            .route("/queue/{id}", delete(remove_queue_item))
            .route("/command", get(commands).post(command))
            .route("/command/{id}", get(command_status))
    };
    let video = || {
        Router::new()
            .route("/customformat", get(custom_formats).post(create_custom_format))
            .route(
                "/customformat/{id}",
                get(custom_format)
                    .put(update_custom_format)
                    .delete(delete_custom_format),
            )
    };
    Router::new()
        .nest(
            "/radarr/api/v3",
            common()
                .merge(video())
                .merge(radarr::routes())
                .layer(Extension(App::Radarr)),
        )
        .nest(
            "/sonarr/api/v3",
            common()
                .merge(video())
                .merge(sonarr::routes())
                .layer(Extension(App::Sonarr)),
        )
        // Bazarr's live updates (RFX-76): a hub that answers, but sends no events yet.
        .nest("/radarr/signalr", signalr::routes())
        .nest("/sonarr/signalr", signalr::routes())
        .nest(
            "/readarr/api/v1",
            common()
                .merge(content::readarr_routes())
                .layer(Extension(App::Readarr)),
        )
        .nest(
            "/lidarr/api/v1",
            common()
                .merge(content::lidarr_routes())
                .layer(Extension(App::Lidarr)),
        )
}

// ---------------------------------------------------------------- helpers

pub(crate) type Reply = ApiResult<Json<Value>>;

pub(crate) fn ok(v: Value) -> Reply {
    Ok(Json(v))
}

pub(crate) fn authorize(user: &CurrentUser) -> ApiResult<()> {
    user.require(Permissions::MANAGE_LIBRARY)
}

/// Settings writes (profiles, definitions, custom formats) need ADMIN, as in the native API.
pub(crate) fn authorize_admin(user: &CurrentUser) -> ApiResult<()> {
    user.require(Permissions::ADMIN)
}

pub(crate) fn library(state: &AppState) -> ApiResult<Arc<LibraryService>> {
    state.ext::<LibraryService>()
}

pub(crate) fn providers(state: &AppState) -> ApiResult<Arc<ProviderService>> {
    state.ext::<ProviderService>()
}

pub(crate) fn activity(state: &AppState) -> ApiResult<Arc<ActivityService>> {
    state.ext::<ActivityService>()
}

/// *arr paging query (`page`, `pageSize`, `sortKey`, `sortDirection`, plus filters).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub sort_key: Option<String>,
    pub sort_direction: Option<String>,
    pub include_series: Option<bool>,
    pub monitored: Option<bool>,
}

/// The `{page, pageSize, totalRecords, records}` envelope.
pub(crate) fn paged(q: &PageQuery, total: u64, records: Vec<Value>) -> Value {
    json!({
        "page": q.page.unwrap_or(1).max(1),
        "pageSize": q.page_size.unwrap_or(10),
        "sortKey": q.sort_key.clone().unwrap_or_default(),
        "sortDirection": q.sort_direction.clone().unwrap_or_else(|| "default".into()),
        "totalRecords": total,
        "records": records,
    })
}

pub(crate) fn date(d: Option<chrono::NaiveDate>) -> Value {
    d.map_or(Value::Null, |d| json!(format!("{d}T00:00:00Z")))
}

pub(crate) fn images(poster: &Option<String>, backdrop: &Option<String>, logo: &Option<String>) -> Value {
    let mut out = Vec::new();
    for (kind, url) in [("poster", poster), ("fanart", backdrop), ("clearlogo", logo)] {
        if let Some(u) = url {
            out.push(json!({"coverType": kind, "url": u, "remoteUrl": u}));
        }
    }
    Value::Array(out)
}

fn resolution(q: Quality) -> u32 {
    let name = q.name();
    for r in ["2160", "1080", "720", "576", "480"] {
        if name.contains(r) {
            return r.parse().unwrap_or(0);
        }
    }
    0
}

fn source(q: Quality) -> &'static str {
    let n = q.name().to_ascii_lowercase();
    if n.starts_with("webdl") {
        "web"
    } else if n.starts_with("webrip") {
        "webRip"
    } else if n.starts_with("bluray") || n.starts_with("remux") || n == "br-disk" {
        "bluray"
    } else if n.starts_with("hdtv") || n == "sdtv" {
        "television"
    } else if n.starts_with("dvd") {
        "dvd"
    } else if n == "cam" || n == "telesync" || n == "telecine" || n == "workprint" {
        "cam"
    } else {
        "unknown"
    }
}

/// The *arr name of a quality (Sonarr calls remuxes `Bluray-1080p Remux`).
pub(crate) fn quality_name(app: App, q: Quality) -> String {
    match (app, q) {
        (App::Sonarr, Quality::Remux1080p) => "Bluray-1080p Remux".into(),
        (App::Sonarr, Quality::Remux2160p) => "Bluray-2160p Remux".into(),
        _ => q.name().into(),
    }
}

pub(crate) fn quality_ref(app: App, q: Quality) -> Value {
    json!({
        "id": q.id(),
        "name": quality_name(app, q),
        "source": source(q),
        "resolution": resolution(q),
        "modifier": if q.name().starts_with("Remux") { "remux" } else { "none" },
    })
}

pub(crate) fn quality_model(app: App, q: &QualityModel) -> Value {
    json!({
        "quality": quality_ref(app, q.quality),
        "revision": {
            "version": q.revision.version.max(1),
            "real": q.revision.real,
            "isRepack": q.revision.is_repack,
        },
    })
}

/// Now plus an ETA from a download client; `None` when the ETA is too large to be a date.
fn completion_time(eta_secs: u64) -> Option<chrono::DateTime<Utc>> {
    let eta = chrono::Duration::try_seconds(i64::try_from(eta_secs).ok()?)?;
    Utc::now().checked_add_signed(eta)
}

fn hms(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

pub(crate) fn media_info(m: &Option<MediaInfoDto>) -> Value {
    let Some(m) = m else {
        return Value::Null;
    };
    json!({
        "audioBitrate": 0,
        "audioChannels": m.audio_channels.unwrap_or(0.0),
        "audioCodec": m.audio_codec.clone().unwrap_or_default(),
        "audioLanguages": m.audio_languages.join("/"),
        "audioStreamCount": m.audio_stream_count,
        "videoBitDepth": m.bit_depth.unwrap_or(0),
        "videoBitrate": m.video_bitrate.unwrap_or(0),
        "videoCodec": m.video_codec.clone().unwrap_or_default(),
        "videoFps": m.fps.unwrap_or(0.0),
        "videoDynamicRange": if m.hdr.is_empty() { "" } else { "HDR" },
        "videoDynamicRangeType": m.hdr.join(" "),
        "resolution": match (m.width, m.height) {
            (Some(w), Some(h)) => format!("{w}x{h}"),
            _ => String::new(),
        },
        "runTime": m.duration_secs.map(|s| hms(s as u64)).unwrap_or_default(),
        "scanType": "Progressive",
        "subtitles": m.subtitle_languages.join("/"),
    })
}

/// Fields shared by `moviefile`, `episodefile`.
pub(crate) fn media_file(app: App, f: &MediaFileDto) -> serde_json::Map<String, Value> {
    let v = json!({
        "id": f.id,
        "relativePath": f.relative_path,
        "path": f.path,
        "size": f.size,
        "dateAdded": f.date_added,
        "sceneName": f.scene_name,
        "releaseGroup": f.release_group,
        "edition": f.edition,
        "languages": f.languages.iter().map(|l| json!({"id": l.id(), "name": l.name()})).collect::<Vec<_>>(),
        "quality": quality_model(app, &f.quality),
        "customFormats": f.custom_formats.iter().enumerate().map(|(i, n)| json!({
            "id": f.custom_format_ids.get(i).copied().unwrap_or(0), "name": n
        })).collect::<Vec<_>>(),
        "customFormatScore": f.custom_format_score,
        "mediaInfo": media_info(&f.media_info),
        "originalFilePath": f.original_path,
        "qualityCutoffNotMet": !f.cutoff_met,
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

/// Reads `?deleteFiles=` and similar flags (`true`/`false`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteQuery {
    #[serde(default)]
    pub delete_files: bool,
    #[serde(default)]
    pub remove_from_client: Option<bool>,
    #[serde(default)]
    pub blocklist: bool,
    #[serde(default)]
    pub skip_redownload: bool,
}

pub(crate) fn created(v: Value) -> ApiResult<Response> {
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

// ---------------------------------------------------------------- system

async fn system_status(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
) -> Reply {
    authorize(&user)?;
    let c = state.config();
    ok(json!({
        "appName": app.name(),
        "instanceName": "Reelix",
        "version": app.version(),
        "buildTime": c.start_time,
        "isDebug": false,
        "isProduction": true,
        "isAdmin": user.is_admin(),
        "isUserInteractive": false,
        "startupPath": "",
        "appData": c.data_dir.to_string_lossy(),
        "osName": std::env::consts::OS,
        "osVersion": "",
        "isNetCore": true,
        "isLinux": cfg!(target_os = "linux"),
        "isOsx": cfg!(target_os = "macos"),
        "isWindows": cfg!(windows),
        "isDocker": std::path::Path::new("/.dockerenv").exists(),
        "mode": "console",
        "branch": "main",
        "authentication": "forms",
        "databaseType": "sqLite",
        "databaseVersion": "3",
        "urlBase": c.url_base,
        "runtimeVersion": env!("CARGO_PKG_VERSION"),
        "runtimeName": "reelix",
        "startTime": c.start_time,
        "packageUpdateMechanism": "external",
    }))
}

/// Reelix's own health checks (`/api/v1/system/health`) in the *arr shape. Like the *arr apps,
/// only problems are listed, so a healthy Reelix answers an empty list. Every check is listed
/// under every app, since the database, data folder, indexers and download clients are shared.
async fn health(State(state): State<AppState>, user: CurrentUser) -> Reply {
    authorize(&user)?;
    let checks = crate::routes::system::run_health_checks(&state).await;
    ok(Value::Array(arr_health(&checks)))
}

/// `[{id, source, type, message, wikiUrl}]` for the checks that are not `ok`; `type` is
/// `warning` or `error`, as in Sonarr's and Radarr's `HealthCheckResult`.
fn arr_health(checks: &[HealthCheck]) -> Vec<Value> {
    checks
        .iter()
        .filter(|c| c.level != HealthLevel::Ok)
        .enumerate()
        .map(|(i, c)| {
            json!({
                "id": i + 1,
                "source": c.source,
                "type": match c.level {
                    HealthLevel::Error => "error",
                    _ => "warning",
                },
                "message": c.message,
                "wikiUrl": "",
            })
        })
        .collect()
}

async fn content_roots(
    state: &AppState,
    app: App,
) -> ApiResult<Vec<(i64, String, bool, Option<u64>, Option<u64>)>> {
    let mut out = Vec::new();
    match app {
        App::Radarr | App::Sonarr => {
            let kind = if app == App::Radarr {
                MediaKind::Movie
            } else {
                MediaKind::Series
            };
            for r in library(state)?.root_folders().await? {
                if r.media_kind == kind {
                    out.push((r.id, r.path, r.accessible, r.free_space, r.total_space));
                }
            }
        }
        App::Readarr => {
            for r in state.ext::<reelix_books::BooksService>()?.root_folders().await? {
                if app.root_kinds().contains(&r.kind) {
                    out.push((r.id, r.path, r.accessible, r.free_space, r.total_space));
                }
            }
        }
        App::Lidarr => {
            for r in state.ext::<reelix_music::MusicService>()?.root_folders().await? {
                out.push((r.id, r.path, r.accessible, r.free_space, r.total_space));
            }
        }
    }
    Ok(out)
}

async fn root_folders(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
) -> Reply {
    authorize(&user)?;
    let roots = content_roots(&state, app).await?;
    ok(Value::Array(
        roots
            .into_iter()
            .map(|(id, path, accessible, free, total)| {
                json!({
                    "id": id,
                    "path": path,
                    "name": path,
                    "accessible": accessible,
                    "freeSpace": free.unwrap_or(0),
                    "totalSpace": total.unwrap_or(0),
                    "unmappedFolders": [],
                })
            })
            .collect(),
    ))
}

async fn diskspace(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
) -> Reply {
    authorize(&user)?;
    let roots = content_roots(&state, app).await?;
    ok(Value::Array(
        roots
            .into_iter()
            .map(|(_, path, _, free, total)| {
                json!({"path": path, "label": "", "freeSpace": free.unwrap_or(0), "totalSpace": total.unwrap_or(0)})
            })
            .collect(),
    ))
}

/// The root folder whose path matches `path` (for `rootFolderPath` in add requests).
pub(crate) async fn root_folder_id(state: &AppState, app: App, path: &str) -> ApiResult<i64> {
    let wanted = path.trim().trim_end_matches(['/', '\\']);
    content_roots(state, app)
        .await?
        .into_iter()
        .find(|(_, p, ..)| p.trim_end_matches(['/', '\\']) == wanted)
        .map(|(id, ..)| id)
        .ok_or_else(|| ApiError::validation(format!("no root folder {path:?} in Reelix")))
}

// ---------------------------------------------------------------- tags

async fn tags(State(state): State<AppState>, user: CurrentUser) -> Reply {
    authorize(&user)?;
    ok(json!(
        providers(&state)?
            .tags()
            .into_iter()
            .map(|t| json!({"id": t.id, "label": t.label}))
            .collect::<Vec<_>>()
    ))
}

async fn tag(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    let t = providers(&state)?
        .tags()
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::not_found("tag not found"))?;
    ok(json!({"id": t.id, "label": t.label}))
}

#[derive(Deserialize)]
struct TagBody {
    label: String,
}

async fn create_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(b): Json<TagBody>,
) -> ApiResult<Response> {
    authorize(&user)?;
    let p = providers(&state)?;
    let t = match p
        .tags()
        .into_iter()
        .find(|t| t.label.eq_ignore_ascii_case(b.label.trim()))
    {
        Some(t) => t,
        None => p
            .create_tag(b.label.trim())
            .await
            .map_err(|e| ApiError::validation(e.to_string()))?,
    };
    created(json!({"id": t.id, "label": t.label}))
}

// ---------------------------------------------------------------- quality profiles

fn profile_json(app: App, p: &QualityProfile, formats: &[reelix_decision::format::CustomFormat]) -> Value {
    let items: Vec<Value> = p
        .items
        .iter()
        .map(|i| match (&i.quality, i.id) {
            (Some(q), _) => json!({"quality": quality_ref(app, *q), "items": [], "allowed": i.allowed}),
            (None, id) => json!({
                "id": id.unwrap_or(0),
                "name": i.name.clone().unwrap_or_default(),
                "items": i.items.iter().map(|q| json!({"quality": quality_ref(app, *q), "items": [], "allowed": i.allowed})).collect::<Vec<_>>(),
                "allowed": i.allowed,
            }),
        })
        .collect();
    let format_items: Vec<Value> = formats
        .iter()
        .map(|f| {
            let score = p
                .format_items
                .iter()
                .find(|fi| fi.format_id == f.id)
                .map_or(0, |fi| fi.score);
            json!({"format": f.id, "name": f.name, "score": score})
        })
        .collect();
    json!({
        "id": p.id,
        "name": p.name,
        "upgradeAllowed": p.upgrade_allowed,
        "cutoff": p.cutoff,
        "items": items,
        "minFormatScore": p.min_format_score,
        "cutoffFormatScore": p.cutoff_format_score,
        "minUpgradeFormatScore": p.min_upgrade_format_score,
        "formatItems": format_items,
        "language": {"id": 1, "name": "English"},
    })
}

async fn video_formats(state: &AppState, app: App) -> ApiResult<Vec<reelix_decision::format::CustomFormat>> {
    if app.family() == QualityFamily::Video {
        Ok(state.db().custom_formats().list().await?)
    } else {
        Ok(Vec::new())
    }
}

async fn profiles_of(state: &AppState, app: App) -> ApiResult<Vec<QualityProfile>> {
    let db = state.db();
    let mut list = db.quality_profiles().list_family(app.family()).await?;
    if app == App::Readarr {
        list.extend(
            db.quality_profiles()
                .list_family(QualityFamily::Audiobook)
                .await?,
        );
    }
    Ok(list)
}

async fn quality_profiles(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
) -> Reply {
    authorize(&user)?;
    let formats = video_formats(&state, app).await?;
    let list = profiles_of(&state, app).await?;
    ok(Value::Array(
        list.iter().map(|p| profile_json(app, p, &formats)).collect(),
    ))
}

async fn quality_profile(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Reply {
    authorize(&user)?;
    let formats = video_formats(&state, app).await?;
    let p = profiles_of(&state, app)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| ApiError::not_found("quality profile not found"))?;
    ok(profile_json(app, &p, &formats))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileBody {
    name: String,
    #[serde(default)]
    upgrade_allowed: bool,
    #[serde(default)]
    cutoff: u32,
    #[serde(default)]
    items: Vec<ProfileItemBody>,
    #[serde(default)]
    min_format_score: i32,
    #[serde(default)]
    cutoff_format_score: i32,
    #[serde(default)]
    min_upgrade_format_score: Option<i32>,
    #[serde(default)]
    format_items: Vec<FormatItemBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileItemBody {
    id: Option<u32>,
    name: Option<String>,
    quality: Option<QualityRefBody>,
    #[serde(default)]
    items: Vec<ProfileItemBody>,
    #[serde(default)]
    allowed: bool,
}

#[derive(Debug, Deserialize)]
struct QualityRefBody {
    id: Option<u16>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FormatItemBody {
    format: i64,
    #[serde(default)]
    score: i32,
}

fn body_quality(q: &QualityRefBody) -> Option<Quality> {
    q.name
        .as_deref()
        .and_then(|n| {
            let n = n
                .replace("Bluray-1080p Remux", "Remux-1080p")
                .replace("Bluray-2160p Remux", "Remux-2160p");
            Quality::from_name(&n)
        })
        .or_else(|| q.id.and_then(Quality::from_id))
}

/// Applies a Radarr/Sonarr-shaped profile body onto `p`.
async fn apply_profile(state: &AppState, app: App, p: &mut QualityProfile, b: ProfileBody) -> ApiResult<()> {
    let family = app.family();
    let mut items = Vec::new();
    for i in &b.items {
        match &i.quality {
            Some(q) => {
                if let Some(q) = body_quality(q).filter(|q| q.family() == family) {
                    items.push(ProfileItem::quality(q, i.allowed));
                }
            }
            None => {
                let members: Vec<Quality> = i
                    .items
                    .iter()
                    .filter_map(|m| m.quality.as_ref().and_then(body_quality))
                    .filter(|q| q.family() == family)
                    .collect();
                if !members.is_empty() {
                    let id = i.id.unwrap_or(1000 + items.len() as u32).max(1000);
                    items.push(ProfileItem::group(
                        id,
                        i.name.as_deref().unwrap_or_default(),
                        members,
                        i.allowed,
                    ));
                }
            }
        }
    }
    if !items.is_empty() {
        p.items = items;
    }
    p.name = b.name.trim().to_owned();
    p.upgrade_allowed = b.upgrade_allowed;
    // The cutoff is a quality or group id from this API (Reelix ids, Sonarr remux names aside).
    if b.cutoff > 0 {
        p.cutoff = b.cutoff;
    }
    p.min_format_score = b.min_format_score;
    p.cutoff_format_score = b.cutoff_format_score;
    if let Some(m) = b.min_upgrade_format_score {
        p.min_upgrade_format_score = m;
    }
    p.format_items = b
        .format_items
        .into_iter()
        .filter(|f| f.score != 0)
        .map(|f| reelix_decision::FormatItem {
            format_id: f.format,
            score: f.score,
        })
        .collect();
    crate::routes::profiles::validate_profile(state, p).await
}

async fn create_quality_profile(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
    Json(b): Json<ProfileBody>,
) -> ApiResult<Response> {
    authorize_admin(&user)?;
    let first = profiles_of(&state, app).await?.into_iter().next();
    let mut p = first.unwrap_or_else(|| QualityProfile::for_family("", app.family(), &[], Quality::Unknown));
    p.id = 0;
    p.format_items.clear();
    apply_profile(&state, app, &mut p, b).await?;
    let created_profile = state.db().quality_profiles().create(&p).await?;
    let formats = video_formats(&state, app).await?;
    created(profile_json(app, &created_profile, &formats))
}

async fn update_quality_profile(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(b): Json<ProfileBody>,
) -> ApiResult<Response> {
    authorize_admin(&user)?;
    let mut p = profiles_of(&state, app)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| ApiError::not_found("quality profile not found"))?;
    apply_profile(&state, app, &mut p, b).await?;
    let saved = state
        .db()
        .quality_profiles()
        .update(id, &p)
        .await?
        .ok_or_else(|| ApiError::not_found("quality profile not found"))?;
    let formats = video_formats(&state, app).await?;
    Ok((StatusCode::ACCEPTED, Json(profile_json(app, &saved, &formats))).into_response())
}

// ---------------------------------------------------------------- quality definitions

async fn quality_definitions(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
) -> Reply {
    authorize(&user)?;
    let defs = state.db().quality_definitions().list_family(app.family()).await?;
    ok(Value::Array(
        defs.iter()
            .enumerate()
            .map(|(i, d)| {
                json!({
                    "id": d.quality.id(),
                    "quality": quality_ref(app, d.quality),
                    "title": d.title,
                    "weight": i + 1,
                    "minSize": d.min_size,
                    "maxSize": d.max_size,
                    "preferredSize": d.preferred_size,
                })
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DefinitionBody {
    quality: QualityRefBody,
    title: Option<String>,
    min_size: Option<f64>,
    max_size: Option<f64>,
    preferred_size: Option<f64>,
}

async fn update_quality_definitions(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
    Json(body): Json<Vec<DefinitionBody>>,
) -> ApiResult<Response> {
    authorize_admin(&user)?;
    let repo = state.db().quality_definitions();
    let mut defs = repo.list_family(app.family()).await?;
    for b in body {
        let Some(q) = body_quality(&b.quality) else {
            continue;
        };
        if let Some(d) = defs.iter_mut().find(|d| d.quality == q) {
            if let Some(t) = b.title.filter(|t| !t.trim().is_empty()) {
                d.title = t;
            }
            d.min_size = b.min_size.unwrap_or(d.min_size).max(0.0);
            // *arr sends `null` (or a huge number) for "unlimited".
            d.max_size = b.max_size.filter(|m| *m > 0.0 && *m < 10_000.0);
            d.preferred_size = b.preferred_size.filter(|m| *m > 0.0 && *m < 10_000.0);
        }
    }
    repo.update_family(app.family(), &defs).await?;
    Ok((StatusCode::ACCEPTED, Json(json!([]))).into_response())
}

// ---------------------------------------------------------------- custom formats

fn format_json(f: &reelix_decision::format::CustomFormat) -> Value {
    let mut v = reelix_decision::trash::export_json(f, None);
    if let Value::Object(m) = &mut v {
        m.insert("id".into(), json!(f.id));
        m.remove("trash_id");
        m.remove("trash_scores");
    }
    v
}

async fn custom_formats(State(state): State<AppState>, user: CurrentUser) -> Reply {
    authorize(&user)?;
    let list = state.db().custom_formats().list().await?;
    ok(Value::Array(list.iter().map(format_json).collect()))
}

async fn custom_format(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    let f = state
        .db()
        .custom_formats()
        .get(id)
        .await?
        .ok_or_else(|| ApiError::not_found("custom format not found"))?;
    ok(format_json(&f))
}

fn parse_format(body: &Value) -> ApiResult<reelix_decision::format::CustomFormat> {
    let mut list =
        reelix_decision::trash::import_value(body).map_err(|e| ApiError::validation(e.to_string()))?;
    if list.len() != 1 {
        return Err(ApiError::validation("expected one custom format"));
    }
    Ok(list.remove(0).format)
}

async fn create_custom_format(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    authorize_admin(&user)?;
    let f = parse_format(&body)?;
    let created_format = state.db().custom_formats().create(&f, None).await?;
    created(format_json(&created_format))
}

async fn update_custom_format(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    authorize_admin(&user)?;
    let f = parse_format(&body)?;
    let saved = state
        .db()
        .custom_formats()
        .update(id, &f)
        .await?
        .ok_or_else(|| ApiError::not_found("custom format not found"))?;
    Ok((StatusCode::ACCEPTED, Json(format_json(&saved))).into_response())
}

async fn delete_custom_format(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    authorize_admin(&user)?;
    state.db().custom_formats().delete(id).await?;
    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------- queue

/// Which queue items belong to an app.
fn queue_kind(app: App) -> &'static str {
    match app {
        App::Radarr => "movie",
        App::Sonarr => "series",
        App::Readarr => "book",
        App::Lidarr => "album",
    }
}

fn tracked_state(s: &str) -> &'static str {
    match s {
        "import_pending" => "importPending",
        "importing" => "importing",
        "failed_pending" => "failedPending",
        _ => "downloading",
    }
}

pub(crate) fn queue_record(app: App, q: &QueueItemDto) -> Value {
    let status = q.client_state.clone().unwrap_or_else(|| "downloading".into());
    let tracked_status = match (q.state.as_str(), status.as_str()) {
        ("failed_pending", _) | (_, "failed") => "error",
        _ if !q.messages.is_empty() || status == "warning" => "warning",
        _ => "ok",
    };
    let mut v = json!({
        "id": q.id,
        "title": q.title,
        "size": q.size.unwrap_or(0),
        "sizeleft": q.remaining.unwrap_or(0),
        "status": status,
        "trackedDownloadStatus": tracked_status,
        "trackedDownloadState": tracked_state(&q.state),
        "statusMessages": if q.messages.is_empty() { json!([]) } else { json!([{"title": q.title, "messages": q.messages}]) },
        "errorMessage": q.messages.first(),
        "downloadId": q.download_id,
        "protocol": q.protocol,
        "downloadClient": q.download_client,
        "indexer": q.indexer,
        "outputPath": q.output_path,
        "timeleft": q.eta_secs.map(hms),
        "estimatedCompletionTime": q.eta_secs.and_then(completion_time),
        "added": q.added,
        "quality": q.quality.as_ref().map(|m| quality_model(app, m)),
        "customFormats": q.custom_formats.iter().map(|f| json!({"id": f.id, "name": f.name})).collect::<Vec<_>>(),
        "customFormatScore": q.format_score,
    });
    let id = q.media.as_ref().map(|m| m.id);
    let m = v.as_object_mut().expect("object");
    match app {
        App::Radarr => {
            m.insert("movieId".into(), json!(id));
        }
        App::Sonarr => {
            m.insert("seriesId".into(), json!(id));
            if let Some(e) = q.episodes.first() {
                m.insert("episodeId".into(), json!(e.id));
                m.insert("seasonNumber".into(), json!(e.season_number));
                m.insert(
                    "episode".into(),
                    json!({"id": e.id, "seriesId": id, "seasonNumber": e.season_number,
                           "episodeNumber": e.episode_number, "title": e.title}),
                );
            }
        }
        App::Readarr => {
            m.insert("bookId".into(), json!(id));
        }
        App::Lidarr => {
            m.insert("albumId".into(), json!(id));
        }
    }
    v
}

async fn queue_items(state: &AppState, app: App) -> ApiResult<Vec<QueueItemDto>> {
    let page = activity(state)?.queue(Some(1), Some(0)).await?;
    let kind = queue_kind(app);
    Ok(page
        .items
        .into_iter()
        .filter(|q| q.media.as_ref().is_some_and(|m| m.kind == kind))
        .collect())
}

async fn queue(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
    Query(q): Query<PageQuery>,
) -> Reply {
    authorize(&user)?;
    let items = queue_items(&state, app).await?;
    let total = items.len() as u64;
    let size = q.page_size.unwrap_or(10).max(1) as usize;
    let page = q.page.unwrap_or(1).max(1) as usize;
    let records = items
        .iter()
        .skip((page - 1) * size)
        .take(size)
        .map(|i| queue_record(app, i))
        .collect();
    ok(paged(&q, total, records))
}

async fn queue_status(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
) -> Reply {
    authorize(&user)?;
    let items = queue_items(&state, app).await?;
    let errors = items
        .iter()
        .any(|q| q.state == "failed_pending" || q.client_state.as_deref() == Some("failed"));
    let warnings = items.iter().any(|q| !q.messages.is_empty());
    ok(json!({
        "totalCount": items.len(),
        "count": items.len(),
        "unknownCount": 0,
        "errors": errors,
        "warnings": warnings,
        "unknownErrors": false,
        "unknownWarnings": false,
    }))
}

async fn remove_queue_item(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<StatusCode> {
    authorize(&user)?;
    activity(&state)?
        .remove(
            id,
            &QueueRemoveQuery {
                remove_from_client: q.remove_from_client.unwrap_or(true),
                blocklist: q.blocklist,
                skip_redownload: q.skip_redownload,
            },
        )
        .await?;
    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------- commands

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandBody {
    pub name: String,
    #[serde(default)]
    pub movie_ids: Vec<i64>,
    pub movie_id: Option<i64>,
    pub series_id: Option<i64>,
    #[serde(default)]
    pub series_ids: Vec<i64>,
    pub season_number: Option<u32>,
    #[serde(default)]
    pub episode_ids: Vec<i64>,
    pub author_id: Option<i64>,
    #[serde(default)]
    pub book_ids: Vec<i64>,
    pub artist_id: Option<i64>,
    #[serde(default)]
    pub album_ids: Vec<i64>,
}

fn command_json(
    id: i64,
    name: &str,
    status: &str,
    queued: DateTime<Utc>,
    ended: Option<DateTime<Utc>>,
    body: &Value,
) -> Value {
    json!({
        "id": id,
        "name": name,
        "commandName": name,
        "status": status,
        "result": if status == "completed" { "successful" } else { "unknown" },
        "queued": queued,
        "started": queued,
        "ended": ended,
        "trigger": "manual",
        "priority": "normal",
        "body": body,
    })
}

async fn command(
    State(state): State<AppState>,
    Extension(app): Extension<App>,
    user: CurrentUser,
    Json(raw): Json<Value>,
) -> ApiResult<Response> {
    authorize(&user)?;
    let body: CommandBody = serde_json::from_value(raw.clone())
        .map_err(|e| ApiError::validation(format!("invalid command: {e}")))?;
    let job = match app {
        App::Radarr => radarr::command(&state, &body).await?,
        App::Sonarr => sonarr::command(&state, &body).await?,
        App::Readarr | App::Lidarr => content::command(&state, app, &body).await?,
    };
    let now = Utc::now();
    // Already-queued duplicates return no job id; report them as done.
    let (id, status) = match job {
        Some(id) => (id, "queued"),
        None => (0, "completed"),
    };
    Ok((
        StatusCode::CREATED,
        Json(command_json(
            id,
            &body.name,
            status,
            now,
            (status == "completed").then_some(now),
            &raw,
        )),
    )
        .into_response())
}

async fn commands(user: CurrentUser) -> Reply {
    authorize(&user)?;
    ok(json!([]))
}

async fn command_status(State(state): State<AppState>, user: CurrentUser, Path(id): Path<i64>) -> Reply {
    authorize(&user)?;
    let job = state
        .db()
        .jobs()
        .get(id)
        .await?
        .ok_or_else(|| ApiError::not_found("command not found"))?;
    let status = match job.status {
        reelix_core::jobs::JobStatus::Queued => "queued",
        reelix_core::jobs::JobStatus::Running => "started",
        reelix_core::jobs::JobStatus::Completed => "completed",
        reelix_core::jobs::JobStatus::Failed => "failed",
    };
    // Only admins see job payloads (the native job list is admin-only).
    let body = if user.is_admin() {
        job.payload.clone()
    } else {
        json!({})
    };
    ok(command_json(
        job.id,
        &job.kind,
        status,
        job.created_at,
        job.finished_at,
        &body,
    ))
}

/// Runs a scheduled task now (RSS sync and the like).
pub(crate) async fn run_task(state: &AppState, kind: &str) -> ApiResult<Option<i64>> {
    state
        .jobs()
        .run_task_now(kind)
        .await
        .map_err(|e| ApiError::validation(e.to_string()))
}

pub(crate) fn unknown_command(name: &str) -> ApiError {
    ApiError::validation(format!("command {name:?} isn't supported by Reelix"))
}

pub(crate) use axum::routing::get as r_get;

// ---------------------------------------------------------------- history

/// Reelix history event → the *arr `eventType`.
fn history_event(app: App, e: &str) -> &'static str {
    match e {
        "grabbed" => "grabbed",
        "imported" => match app {
            App::Readarr => "bookFileImported",
            App::Lidarr => "trackFileImported",
            _ => "downloadFolderImported",
        },
        "download_failed" => "downloadFailed",
        "import_failed" => match app {
            App::Radarr | App::Sonarr => "downloadFailed",
            _ => "albumImportIncomplete",
        },
        "file_deleted" => match app {
            App::Radarr => "movieFileDeleted",
            App::Sonarr => "episodeFileDeleted",
            App::Readarr => "bookFileDeleted",
            App::Lidarr => "trackFileDeleted",
        },
        "renamed" => match app {
            App::Radarr => "movieFileRenamed",
            App::Sonarr => "episodeFileRenamed",
            App::Readarr => "bookFileRenamed",
            App::Lidarr => "trackFileRenamed",
        },
        "ignored" => "downloadIgnored",
        _ => "unknown",
    }
}

/// An *arr `eventType` filter (name or Radarr/Sonarr number) → the Reelix event.
pub(crate) fn history_event_from_arr(e: &str) -> Option<String> {
    Some(
        match e.trim().to_ascii_lowercase().as_str() {
            "1" | "grabbed" => "grabbed",
            "3" | "downloadfolderimported" | "bookfileimported" | "trackfileimported" => "imported",
            "4" | "downloadfailed" => "download_failed",
            "6" | "moviefiledeleted" | "episodefiledeleted" | "bookfiledeleted" | "trackfiledeleted" => {
                "file_deleted"
            }
            "8" | "moviefilerenamed" | "episodefilerenamed" => "renamed",
            "9" | "downloadignored" => "ignored",
            _ => return None,
        }
        .to_owned(),
    )
}

pub(crate) fn history_record(app: App, h: &reelix_api_types::pipeline::HistoryDto) -> Value {
    let id = h.media.as_ref().map(|m| m.id);
    let mut v = json!({
        "id": h.id,
        "sourceTitle": h.source_title,
        "quality": h.quality.as_ref().map(|q| quality_model(app, q)),
        "customFormats": h.custom_formats.iter().map(|f| json!({"id": f.id, "name": f.name})).collect::<Vec<_>>(),
        "customFormatScore": h.format_score,
        "languages": h.languages.iter().map(|l| json!({"id": l.id(), "name": l.name()})).collect::<Vec<_>>(),
        "date": h.date,
        "eventType": history_event(app, &h.event_type),
        "downloadId": h.download_id,
        "data": {
            "indexer": h.indexer,
            "downloadClient": h.download_client,
            "downloadClientName": h.download_client,
        },
        "qualityCutoffNotMet": false,
    });
    let m = v.as_object_mut().expect("object");
    match app {
        App::Radarr => {
            m.insert("movieId".into(), json!(id));
        }
        App::Sonarr => {
            m.insert("seriesId".into(), json!(id));
            m.insert("episodeId".into(), json!(h.episode.as_ref().map(|e| e.id)));
        }
        App::Readarr => {
            m.insert("bookId".into(), json!(id));
        }
        App::Lidarr => {
            m.insert("albumId".into(), json!(id));
        }
    }
    v
}
