//! Providers: indexers, download clients, notifications, media servers, import lists,
//! tags, remote path mappings, metadata and indexer settings.
//!
//! Every kind gets the same CRUD + schema + test routes under its path segment
//! (`indexers`, `downloadclients`, `notifications`, `mediaservers`, `importlists`).
//! Admin only, except manual indexer search / categories and tag reads/creates
//! (`MANAGE_LIBRARY`).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use reelix_api_types::providers::{
    CategoryDto, IndexerSearchOutcome, IndexerSearchResponse, IndexerSettingsDto, IndexerStatsDto,
    IndexerStatusDto, MetadataSettingsDto, PlexPinDto, PlexPinStatusDto, ProviderDto, ProviderInput,
    ProviderKind, ProviderSchemaDto, ProviderTestRequest, ReleaseDto, RemotePathMappingDto,
    RemotePathMappingInput, TagDto, TagInput, TestResultDto,
};
use reelix_api_types::{ApiErrorBody, HealthCheck};
use reelix_core::{BookFormat, Permissions};
use reelix_indexers::{Category, ReleaseInfo, SearchKind, SearchQuery};
use reelix_providers::{ProviderError, ProviderService, RemotePathMapping};
use serde::Deserialize;
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::routes::system::HealthSource;
use crate::state::AppState;

impl From<ProviderError> for ApiError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::NotFound(m) => ApiError::NotFound(m),
            ProviderError::Validation(m) | ProviderError::NotConfigured(m) => ApiError::validation(m),
            ProviderError::Conflict(m) => ApiError::Conflict(m),
            ProviderError::Upstream(m) => ApiError::Validation {
                status: StatusCode::BAD_GATEWAY,
                message: m,
            },
            ProviderError::Db(e) => e.into(),
        }
    }
}

impl HealthSource for ProviderService {
    fn checks(&self) -> Vec<HealthCheck> {
        self.health()
    }
}

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    let mut r = Router::new()
        .route("/indexers/stats", get(indexer_stats))
        .route("/indexers/search", get(indexer_search))
        .route("/indexers/categories", get(indexer_categories))
        .route("/downloadclients/{id}/categories", get(client_categories))
        .route("/mediaservers/plex/pin", post(plex_pin))
        .route("/mediaservers/plex/pin/{id}", get(plex_pin_status))
        .route("/tags", get(list_tags).post(create_tag))
        .route("/tags/{id}", delete(delete_tag))
        .route("/remotepathmappings", get(list_mappings).post(create_mapping))
        .route(
            "/remotepathmappings/{id}",
            put(update_mapping).delete(delete_mapping),
        )
        .route("/settings/metadata", get(get_metadata).put(put_metadata))
        .route("/settings/metadata/test", post(test_metadata))
        .route(
            "/settings/indexers",
            get(get_indexer_settings).put(put_indexer_settings),
        )
        .merge(definition_routes());
    for kind in ProviderKind::ALL {
        let base = format!("/{}", kind.path());
        r = r
            .route(
                &base,
                get(move |s: State<AppState>, u: CurrentUser| list(s, u, kind)).post(
                    move |s: State<AppState>, u: CurrentUser, b: ApiJson<ProviderInput>| {
                        create(s, u, kind, b)
                    },
                ),
            )
            .route(
                &format!("{base}/schema"),
                get(move |s: State<AppState>, u: CurrentUser| schemas(s, u, kind)),
            )
            .route(
                &format!("{base}/test"),
                post(
                    move |s: State<AppState>, u: CurrentUser, b: ApiJson<ProviderTestRequest>| {
                        test_unsaved(s, u, kind, b)
                    },
                ),
            )
            .route(
                &format!("{base}/{{id}}"),
                get(move |s: State<AppState>, u: CurrentUser, p: ApiPath<i64>| get_one(s, u, kind, p))
                    .put(
                        move |s: State<AppState>,
                              u: CurrentUser,
                              p: ApiPath<i64>,
                              b: ApiJson<ProviderInput>| { update(s, u, kind, p, b) },
                    )
                    .delete(move |s: State<AppState>, u: CurrentUser, p: ApiPath<i64>| remove(s, u, kind, p)),
            )
            .route(
                &format!("{base}/{{id}}/test"),
                post(move |s: State<AppState>, u: CurrentUser, p: ApiPath<i64>| test_saved(s, u, kind, p)),
            );
    }
    r
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list, get_one, create, update, remove, schemas, test_unsaved, test_saved,
        indexer_stats, indexer_search, indexer_categories, client_categories, plex_pin, plex_pin_status,
        list_tags, create_tag, delete_tag, list_mappings, create_mapping, update_mapping, delete_mapping,
        get_metadata, put_metadata, test_metadata, get_indexer_settings, put_indexer_settings,
        sync_definitions, definitions_status, get_definitions_settings, put_definitions_settings,
    ),
    components(schemas(
        ProviderKind, ProviderDto, ProviderInput, ProviderTestRequest, ProviderSchemaDto, TestResultDto,
        reelix_api_types::providers::TestCheckDto, IndexerStatusDto, IndexerStatsDto, ReleaseDto,
        IndexerSearchOutcome, IndexerSearchResponse, CategoryDto, TagDto, TagInput, RemotePathMappingDto,
        RemotePathMappingInput, MetadataSettingsDto, IndexerSettingsDto, reelix_api_types::providers::ProxyDto,
        PlexPinDto, PlexPinStatusDto, reelix_api_types::providers::PlexServerDto,
        DefinitionsSettingsDto, DefinitionsStatusDto, reelix_api_types::providers::DefinitionLoadErrorDto,
        reelix_api_types::providers::DefinitionSource,
    )),
    tags((name = "providers", description = "Indexers, download clients, notifications, media servers, import lists, tags, remote path mappings, metadata and indexer settings")),
)]
pub struct Api;

fn svc(state: &AppState) -> ApiResult<Arc<ProviderService>> {
    state.ext::<ProviderService>()
}

fn created<T: serde::Serialize>(v: T) -> Response {
    (StatusCode::CREATED, ApiJson(v)).into_response()
}

// ---------------- generic provider CRUD ----------------

/// Configured instances of a provider kind (`kind` ∈ indexers, downloadclients, notifications,
/// mediaservers, importlists). Secrets are masked.
#[utoipa::path(get, path = "/api/v1/{kind}", tag = "providers",
    params(("kind" = String, Path, description = "indexers | downloadclients | notifications | mediaservers | importlists")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Instances by priority", body = Vec<ProviderDto>),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    kind: ProviderKind,
) -> ApiResult<ApiJson<Vec<ProviderDto>>> {
    user.require(Permissions::ADMIN)?;
    let s = svc(&state)?;
    Ok(ApiJson(s.list(kind).await?.iter().map(|r| s.to_dto(r)).collect()))
}

/// One instance.
#[utoipa::path(get, path = "/api/v1/{kind}/{id}", tag = "providers",
    params(("kind" = String, Path, description = "Provider kind path"), ("id" = i64, Path, description = "Instance id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Instance", body = ProviderDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn get_one(
    State(state): State<AppState>,
    user: CurrentUser,
    kind: ProviderKind,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<ProviderDto>> {
    user.require(Permissions::ADMIN)?;
    let s = svc(&state)?;
    let row = s
        .get(kind, id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("{} not found", kind.label())))?;
    Ok(ApiJson(s.to_dto(&row)))
}

/// Creates an instance (validated against its schema; secrets encrypted at rest).
#[utoipa::path(post, path = "/api/v1/{kind}", tag = "providers", request_body = ProviderInput,
    params(("kind" = String, Path, description = "Provider kind path")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = ProviderDto),
        (status = 400, description = "Invalid settings", body = ApiErrorBody),
        (status = 409, description = "Duplicate name", body = ApiErrorBody),
    ))]
pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    kind: ProviderKind,
    ApiJson(input): ApiJson<ProviderInput>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let s = svc(&state)?;
    let row = s.create(kind, &input).await?;
    Ok(created(s.to_dto(&row)))
}

/// Replaces an instance. Secret fields sent as the mask keep their stored value.
#[utoipa::path(put, path = "/api/v1/{kind}/{id}", tag = "providers", request_body = ProviderInput,
    params(("kind" = String, Path, description = "Provider kind path"), ("id" = i64, Path, description = "Instance id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = ProviderDto),
        (status = 400, description = "Invalid settings", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    kind: ProviderKind,
    ApiPath(id): ApiPath<i64>,
    ApiJson(input): ApiJson<ProviderInput>,
) -> ApiResult<ApiJson<ProviderDto>> {
    user.require(Permissions::ADMIN)?;
    let s = svc(&state)?;
    let row = s.update(kind, id, &input).await?;
    Ok(ApiJson(s.to_dto(&row)))
}

/// Deletes an instance.
#[utoipa::path(delete, path = "/api/v1/{kind}/{id}", tag = "providers",
    params(("kind" = String, Path, description = "Provider kind path"), ("id" = i64, Path, description = "Instance id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn remove(
    State(state): State<AppState>,
    user: CurrentUser,
    kind: ProviderKind,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    if svc(&state)?.delete(kind, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("{} not found", kind.label())))
    }
}

/// Implementations that can be added (indexers include loaded YAML definitions).
#[utoipa::path(get, path = "/api/v1/{kind}/schema", tag = "providers",
    params(("kind" = String, Path, description = "Provider kind path")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Schemas with catalog facets", body = Vec<ProviderSchemaDto>)))]
pub async fn schemas(
    State(state): State<AppState>,
    user: CurrentUser,
    kind: ProviderKind,
) -> ApiResult<ApiJson<Vec<ProviderSchemaDto>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.schemas(kind)))
}

/// Tests unsaved settings; with `id`, masked secrets come from that saved instance.
#[utoipa::path(post, path = "/api/v1/{kind}/test", tag = "providers", request_body = ProviderTestRequest,
    params(("kind" = String, Path, description = "Provider kind path")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Per-check result (ok=false on failure)", body = TestResultDto)))]
pub async fn test_unsaved(
    State(state): State<AppState>,
    user: CurrentUser,
    kind: ProviderKind,
    ApiJson(req): ApiJson<ProviderTestRequest>,
) -> ApiResult<ApiJson<TestResultDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.test_input(kind, &req).await?))
}

/// Tests a saved instance (indexers: also clears back-off on success).
#[utoipa::path(post, path = "/api/v1/{kind}/{id}/test", tag = "providers",
    params(("kind" = String, Path, description = "Provider kind path"), ("id" = i64, Path, description = "Instance id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Per-check result", body = TestResultDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn test_saved(
    State(state): State<AppState>,
    user: CurrentUser,
    kind: ProviderKind,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<TestResultDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.test_saved(kind, id).await?))
}

// ---------------- indexers ----------------

/// Per-indexer counters (since startup) and back-off state.
#[utoipa::path(get, path = "/api/v1/indexers/stats", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Stats for every indexer", body = Vec<IndexerStatsDto>)))]
pub async fn indexer_stats(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<IndexerStatsDto>>> {
    user.require(Permissions::ADMIN)?;
    let s = svc(&state)?;
    let stats = s.indexer_manager().stats();
    let rows = s.list(ProviderKind::Indexer).await?;
    Ok(ApiJson(
        rows.iter()
            .map(|r| match stats.iter().find(|st| st.indexer_id == r.id) {
                Some(st) => IndexerStatsDto {
                    indexer_id: r.id,
                    name: r.name.clone(),
                    enabled: r.enabled,
                    queries: st.queries,
                    rss_queries: st.rss_queries,
                    grabs: st.grabs,
                    failures: st.failures,
                    avg_response_ms: st.avg_response_ms,
                    status: IndexerStatusDto {
                        failure_count: st.status.failure_count,
                        disabled_until: st.status.disabled_until,
                        last_failure_reason: st.status.last_failure_reason.clone(),
                    },
                },
                None => IndexerStatsDto {
                    indexer_id: r.id,
                    name: r.name.clone(),
                    enabled: r.enabled,
                    queries: 0,
                    rss_queries: 0,
                    grabs: 0,
                    failures: 0,
                    avg_response_ms: 0,
                    status: IndexerStatusDto::default(),
                },
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// Search term.
    pub q: Option<String>,
    /// Comma-separated Newznab category ids.
    pub cat: Option<String>,
    /// Comma-separated indexer ids (default: all with interactive search enabled).
    pub ids: Option<String>,
    /// `basic` (default), `movie`, `tv`, `book` or `music`.
    pub kind: Option<String>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub year: Option<u16>,
    pub season: Option<u32>,
    pub episode: Option<String>,
    /// Max results per indexer.
    pub limit: Option<u32>,
    /// Book search: author.
    #[serde(default)]
    pub author: Option<String>,
    /// Book search: title.
    #[serde(default)]
    pub title: Option<String>,
    /// Book search: `ebook` or `audiobook` (default: both).
    #[serde(default)]
    pub format: Option<String>,
    /// Music search: artist.
    #[serde(default)]
    pub artist: Option<String>,
    /// Music search: album.
    #[serde(default)]
    pub album: Option<String>,
}

fn trimmed(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn csv<T: std::str::FromStr>(s: Option<&str>, what: &str) -> ApiResult<Vec<T>> {
    s.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.parse()
                .map_err(|_| ApiError::validation(format!("invalid {what} `{p}`")))
        })
        .collect()
}

fn release_dto(r: ReleaseInfo) -> ReleaseDto {
    ReleaseDto {
        guid: r.guid,
        title: r.title,
        download_url: r.download_url,
        magnet_url: r.magnet_url,
        info_url: r.info_url,
        info_hash: r.info_hash,
        size: r.size,
        publish_date: r.publish_date,
        protocol: r.protocol,
        indexer_id: r.indexer_id,
        indexer: r.indexer,
        categories: r.categories,
        seeders: r.seeders,
        peers: r.peers,
        grabs: r.grabs,
        imdb_id: r.imdb_id,
        tmdb_id: r.tmdb_id,
        tvdb_id: r.tvdb_id,
        download_volume_factor: r.download_volume_factor,
        upload_volume_factor: r.upload_volume_factor,
        indexer_flags: r
            .indexer_flags
            .iter()
            .filter_map(|f| serde_json::to_value(f).ok()?.as_str().map(String::from))
            .collect(),
    }
}

/// Manual multi-indexer search → releases (newest first) plus per-indexer outcome.
#[utoipa::path(get, path = "/api/v1/indexers/search", tag = "providers", params(SearchParams),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Releases and per-indexer results", body = IndexerSearchResponse),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
    ))]
pub async fn indexer_search(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(p): ApiQuery<SearchParams>,
) -> ApiResult<ApiJson<IndexerSearchResponse>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    let ids: Vec<i64> = csv(p.ids.as_deref(), "indexer id")?;
    let kind = match p.kind.as_deref().unwrap_or("basic") {
        "basic" | "" => SearchKind::Basic,
        "movie" => SearchKind::Movie,
        "tv" => SearchKind::Tv,
        "book" => SearchKind::Book,
        "music" => SearchKind::Music,
        other => return Err(ApiError::validation(format!("unknown search kind `{other}`"))),
    };
    let query = SearchQuery {
        kind,
        term: p.q.map(|q| q.trim().to_owned()).filter(|q| !q.is_empty()),
        imdb_id: p.imdb_id,
        tmdb_id: p.tmdb_id,
        tvdb_id: p.tvdb_id,
        year: p.year,
        season: p.season,
        episode: p.episode,
        categories: csv(p.cat.as_deref(), "category")?,
        limit: p.limit.map(|l| l.clamp(1, 1000)),
        offset: None,
        author: trimmed(p.author),
        title: trimmed(p.title),
        artist: trimmed(p.artist),
        album: trimmed(p.album),
        book_format: match p.format.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(f) => Some(
                BookFormat::parse(f)
                    .ok_or_else(|| ApiError::validation(format!("unknown book format `{f}`")))?,
            ),
        },
    };
    let s = svc(&state)?;
    let results = s
        .search(&query, (!ids.is_empty()).then_some(ids.as_slice()), true)
        .await;
    let mut releases = Vec::new();
    let mut indexers = Vec::new();
    for r in results {
        let elapsed_ms = r.elapsed.as_millis() as u64;
        match r.result {
            Ok(list) => {
                indexers.push(IndexerSearchOutcome {
                    indexer_id: r.indexer_id,
                    name: r.name,
                    count: list.len(),
                    elapsed_ms,
                    error: None,
                });
                releases.extend(list.into_iter().map(release_dto));
            }
            Err(e) => indexers.push(IndexerSearchOutcome {
                indexer_id: r.indexer_id,
                name: r.name,
                count: 0,
                elapsed_ms,
                error: Some(e.to_string()),
            }),
        }
    }
    releases.sort_by_key(|r| std::cmp::Reverse(r.publish_date));
    Ok(ApiJson(IndexerSearchResponse { releases, indexers }))
}

fn category_dto(c: Category) -> CategoryDto {
    CategoryDto {
        id: c.id,
        name: c.name,
        subcategories: c.subcategories.into_iter().map(category_dto).collect(),
    }
}

/// Standard Newznab categories (search page picker).
#[utoipa::path(get, path = "/api/v1/indexers/categories", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Category tree", body = Vec<CategoryDto>)))]
pub async fn indexer_categories(user: CurrentUser) -> ApiResult<ApiJson<Vec<CategoryDto>>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    Ok(ApiJson(
        reelix_indexers::categories::standard()
            .into_iter()
            .map(category_dto)
            .collect(),
    ))
}

// ---------------- download clients / media servers ----------------

/// Categories / labels known to a download client.
#[utoipa::path(get, path = "/api/v1/downloadclients/{id}/categories", tag = "providers",
    params(("id" = i64, Path, description = "Download client id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Category names", body = Vec<String>),
        (status = 502, description = "Client unreachable", body = ApiErrorBody),
    ))]
pub async fn client_categories(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<Vec<String>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.download_client_categories(id).await?))
}

/// Starts a plex.tv PIN login (to obtain a server token).
#[utoipa::path(post, path = "/api/v1/mediaservers/plex/pin", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "PIN; open auth_url in a popup", body = PlexPinDto)))]
pub async fn plex_pin(State(state): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<PlexPinDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.plex_create_pin().await?))
}

/// Polls a plex.tv PIN; once authorized returns the token and the account's servers.
#[utoipa::path(get, path = "/api/v1/mediaservers/plex/pin/{id}", tag = "providers",
    params(("id" = i64, Path, description = "PIN id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "PIN status", body = PlexPinStatusDto)))]
pub async fn plex_pin_status(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<PlexPinStatusDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.plex_check_pin(id).await?))
}

// ---------------- tags ----------------

/// All tags.
#[utoipa::path(get, path = "/api/v1/tags", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Tags by label", body = Vec<TagDto>)))]
pub async fn list_tags(State(state): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<Vec<TagDto>>> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    Ok(ApiJson(
        svc(&state)?
            .tags()
            .into_iter()
            .map(|t| TagDto {
                id: t.id,
                label: t.label,
            })
            .collect(),
    ))
}

/// Creates a tag (labels are unique, case-insensitive).
#[utoipa::path(post, path = "/api/v1/tags", tag = "providers", request_body = TagInput,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = TagDto),
        (status = 409, description = "Duplicate label", body = ApiErrorBody),
    ))]
pub async fn create_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(t): ApiJson<TagInput>,
) -> ApiResult<Response> {
    user.require(Permissions::MANAGE_LIBRARY)?;
    let tag = svc(&state)?.create_tag(&t.label).await?;
    Ok(created(TagDto {
        id: tag.id,
        label: tag.label,
    }))
}

/// Deletes a tag and removes it from every provider (admin).
#[utoipa::path(delete, path = "/api/v1/tags/{id}", tag = "providers",
    params(("id" = i64, Path, description = "Tag id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    if svc(&state)?.delete_tag(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("tag not found"))
    }
}

// ---------------- remote path mappings ----------------

fn mapping_dto(m: RemotePathMapping) -> RemotePathMappingDto {
    RemotePathMappingDto {
        id: m.id,
        host: m.host,
        remote_path: m.remote_path,
        local_path: m.local_path,
    }
}

/// Remote path mappings.
#[utoipa::path(get, path = "/api/v1/remotepathmappings", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Mappings", body = Vec<RemotePathMappingDto>)))]
pub async fn list_mappings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<RemotePathMappingDto>>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(
        svc(&state)?
            .remote_path_mappings()
            .into_iter()
            .map(mapping_dto)
            .collect(),
    ))
}

/// Adds a remote path mapping.
#[utoipa::path(post, path = "/api/v1/remotepathmappings", tag = "providers", request_body = RemotePathMappingInput,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = RemotePathMappingDto),
        (status = 400, description = "Invalid", body = ApiErrorBody),
        (status = 409, description = "Duplicate", body = ApiErrorBody),
    ))]
pub async fn create_mapping(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(m): ApiJson<RemotePathMappingInput>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let m = svc(&state)?
        .create_mapping(&m.host, &m.remote_path, &m.local_path)
        .await?;
    Ok(created(mapping_dto(m)))
}

/// Replaces a remote path mapping.
#[utoipa::path(put, path = "/api/v1/remotepathmappings/{id}", tag = "providers", request_body = RemotePathMappingInput,
    params(("id" = i64, Path, description = "Mapping id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = RemotePathMappingDto),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn update_mapping(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(m): ApiJson<RemotePathMappingInput>,
) -> ApiResult<ApiJson<RemotePathMappingDto>> {
    user.require(Permissions::ADMIN)?;
    let m = svc(&state)?
        .update_mapping(id, &m.host, &m.remote_path, &m.local_path)
        .await?;
    Ok(ApiJson(mapping_dto(m)))
}

/// Deletes a remote path mapping.
#[utoipa::path(delete, path = "/api/v1/remotepathmappings/{id}", tag = "providers",
    params(("id" = i64, Path, description = "Mapping id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_mapping(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    if svc(&state)?.delete_mapping(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("remote path mapping not found"))
    }
}

// ---------------- settings ----------------

/// Metadata settings (keys masked).
#[utoipa::path(get, path = "/api/v1/settings/metadata", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Settings", body = MetadataSettingsDto)))]
pub async fn get_metadata(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<MetadataSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.metadata_settings()))
}

/// Saves metadata settings (mask = unchanged).
#[utoipa::path(put, path = "/api/v1/settings/metadata", tag = "providers", request_body = MetadataSettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved (masked)", body = MetadataSettingsDto),
        (status = 400, description = "Invalid", body = ApiErrorBody),
    ))]
pub async fn put_metadata(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(m): ApiJson<MetadataSettingsDto>,
) -> ApiResult<ApiJson<MetadataSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_metadata_settings(m).await?))
}

/// Tests (unsaved) metadata settings against TMDB / TVDB.
#[utoipa::path(post, path = "/api/v1/settings/metadata/test", tag = "providers", request_body = MetadataSettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Per-provider checks", body = TestResultDto)))]
pub async fn test_metadata(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(m): ApiJson<MetadataSettingsDto>,
) -> ApiResult<ApiJson<TestResultDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.test_metadata(m).await?))
}

/// Indexer-wide settings (proxy password masked).
#[utoipa::path(get, path = "/api/v1/settings/indexers", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Settings", body = IndexerSettingsDto)))]
pub async fn get_indexer_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<IndexerSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.indexer_settings()))
}

/// Saves indexer-wide settings; rebuilds every indexer.
#[utoipa::path(put, path = "/api/v1/settings/indexers", tag = "providers", request_body = IndexerSettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved (masked)", body = IndexerSettingsDto),
        (status = 400, description = "Invalid", body = ApiErrorBody),
    ))]
pub async fn put_indexer_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<IndexerSettingsDto>,
) -> ApiResult<ApiJson<IndexerSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_indexer_settings(s).await?))
}

// ---------------- indexer definition catalog ----------------

use reelix_api_types::EnqueuedJob;
use reelix_api_types::providers::{DefinitionsSettingsDto, DefinitionsStatusDto};
use reelix_providers::jobs::DEFINITIONS_SYNC;

fn definition_routes() -> Router<AppState> {
    Router::new()
        .route("/indexers/definitions/sync", post(sync_definitions))
        .route("/indexers/definitions/status", get(definitions_status))
        .route(
            "/settings/definitions",
            get(get_definitions_settings).put(put_definitions_settings),
        )
}

/// Downloads the indexer definition catalog now (ignores the update interval). Progress is
/// published as `entity.changed` `indexer_definitions` (`syncing`, then `synced` / `failed`).
#[utoipa::path(post, path = "/api/v1/indexers/definitions/sync", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Sync queued", body = EnqueuedJob),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn sync_definitions(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    let job_id = state
        .jobs()
        .enqueue(
            reelix_db::NewJob::new(DEFINITIONS_SYNC, serde_json::json!({ "force": true }))
                .priority(5)
                .dedupe("definitions_sync:force"),
        )
        .await?;
    Ok((StatusCode::ACCEPTED, ApiJson(EnqueuedJob { job_id })).into_response())
}

/// Definition catalog status: last sync, counts and definitions that failed to load.
#[utoipa::path(get, path = "/api/v1/indexers/definitions/status", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Status", body = DefinitionsStatusDto),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn definitions_status(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<DefinitionsStatusDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.definitions_status()))
}

/// Definition catalog sync settings.
#[utoipa::path(get, path = "/api/v1/settings/definitions", tag = "providers",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Settings", body = DefinitionsSettingsDto)))]
pub async fn get_definitions_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<DefinitionsSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.definitions_settings()))
}

/// Saves the sync settings (source URL, schema version, auto-update + interval); the sync
/// state fields are read-only.
#[utoipa::path(put, path = "/api/v1/settings/definitions", tag = "providers", request_body = DefinitionsSettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = DefinitionsSettingsDto),
        (status = 400, description = "Invalid", body = ApiErrorBody),
    ))]
pub async fn put_definitions_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<DefinitionsSettingsDto>,
) -> ApiResult<ApiJson<DefinitionsSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_definitions_settings(s).await?))
}
