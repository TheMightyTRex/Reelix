//! Import list sync (`import_list_sync`) and the exclusion / status tables.

use std::collections::HashSet;

use reelix_api_types::integrations::{CreateExclusionRequest, ImportListExclusionDto, ImportListStatusDto};
use reelix_api_types::library::{
    AddMovieRequest, AddSeriesRequest, MinimumAvailability, MovieMonitor, NewItemMonitor, RootFolderDto,
    SeriesMonitor, SeriesType,
};
use reelix_core::{DomainEvent, MediaKind};
use reelix_db::{Db, from_ms, now_ms};
use reelix_library::LibraryError;
use reelix_lists::ListItem;
use reelix_metadata::ExternalSource;
use reelix_providers::ProviderRow;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::IntegrationsService;

/// Job payload of `import_list_sync`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct SyncPayload {
    /// Only these lists (empty = all enabled lists).
    pub list_ids: Vec<i64>,
    /// Ignore each list's sync interval.
    pub force: bool,
}

/// Per-list add options (fields added to every import-list schema by the providers crate).
#[derive(Debug, Clone, PartialEq)]
pub struct ListOptions {
    pub enabled_auto_add: bool,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub monitor: MovieMonitor,
    pub series_monitor: SeriesMonitor,
    pub minimum_availability: MinimumAvailability,
    pub series_type: SeriesType,
    pub season_folder: bool,
    pub search_on_add: bool,
    pub tags: Vec<i64>,
}

fn enum_setting<T: DeserializeOwned + Default>(s: &Map<String, Value>, key: &str) -> T {
    s.get(key)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn id_setting(s: &Map<String, Value>, key: &str) -> Option<i64> {
    match s.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
    .filter(|id| *id > 0)
}

impl ListOptions {
    pub fn from_row(row: &ProviderRow) -> Self {
        let s = &row.settings;
        Self {
            enabled_auto_add: row.setting_bool("enabled_auto_add", true),
            quality_profile_id: id_setting(s, "quality_profile_id"),
            root_folder_id: id_setting(s, "root_folder_id"),
            monitor: enum_setting(s, "monitor"),
            series_monitor: enum_setting(s, "series_monitor"),
            minimum_availability: enum_setting(s, "minimum_availability"),
            series_type: enum_setting(s, "series_type"),
            season_folder: row.setting_bool("season_folder", true),
            search_on_add: row.setting_bool("search_on_add", true),
            tags: row.tags.clone(),
        }
    }
}

/// Counts of one list's sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListSyncOutcome {
    pub list_id: i64,
    pub found: u32,
    pub added: u32,
    pub excluded: u32,
    pub existing: u32,
    pub unresolved: u32,
    pub error: Option<String>,
}

fn kind_str(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
    }
}

fn parse_kind(s: &str) -> MediaKind {
    if s == "series" {
        MediaKind::Series
    } else {
        MediaKind::Movie
    }
}

// ---------------------------------------------------------------- exclusions / status repo

type ExclusionRow = (i64, String, i64, String, Option<i64>, i64);

fn exclusion_dto(r: ExclusionRow) -> ImportListExclusionDto {
    ImportListExclusionDto {
        id: r.0,
        kind: parse_kind(&r.1),
        tmdb_id: r.2 as u32,
        title: r.3,
        year: r.4.map(|y| y as i32),
        created_at: from_ms(r.5),
    }
}

pub async fn list_exclusions(db: &Db) -> sqlx::Result<Vec<ImportListExclusionDto>> {
    let rows: Vec<ExclusionRow> = sqlx::query_as(
        "SELECT id, media_kind, tmdb_id, title, year, created_at FROM import_list_exclusions
         ORDER BY created_at DESC, id DESC",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(exclusion_dto).collect())
}

/// Inserts an exclusion; `None` when the media is already excluded.
pub async fn add_exclusion(
    db: &Db,
    req: &CreateExclusionRequest,
) -> sqlx::Result<Option<ImportListExclusionDto>> {
    let row: Option<ExclusionRow> = sqlx::query_as(
        "INSERT INTO import_list_exclusions (media_kind, tmdb_id, title, year, created_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (media_kind, tmdb_id) DO NOTHING
         RETURNING id, media_kind, tmdb_id, title, year, created_at",
    )
    .bind(kind_str(req.kind))
    .bind(i64::from(req.tmdb_id))
    .bind(req.title.trim())
    .bind(req.year)
    .bind(now_ms())
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(exclusion_dto))
}

/// Deletes the given exclusions (empty = all).
pub async fn delete_exclusions(db: &Db, ids: &[i64]) -> sqlx::Result<u64> {
    if ids.is_empty() {
        return Ok(sqlx::query("DELETE FROM import_list_exclusions")
            .execute(db.pool())
            .await?
            .rows_affected());
    }
    let mut n = 0;
    for id in ids {
        n += sqlx::query("DELETE FROM import_list_exclusions WHERE id = ?")
            .bind(id)
            .execute(db.pool())
            .await?
            .rows_affected();
    }
    Ok(n)
}

async fn excluded_set(db: &Db) -> sqlx::Result<HashSet<(MediaKind, u32)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT media_kind, tmdb_id FROM import_list_exclusions")
        .fetch_all(db.pool())
        .await?;
    Ok(rows
        .into_iter()
        .map(|(k, id)| (parse_kind(&k), id as u32))
        .collect())
}

pub async fn list_status(db: &Db) -> sqlx::Result<Vec<ImportListStatusDto>> {
    let rows: Vec<(i64, i64, Option<String>, i64, i64)> = sqlx::query_as(
        "SELECT list_id, last_sync, last_error, items_found, items_added FROM import_list_status ORDER BY list_id",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ImportListStatusDto {
            list_id: r.0,
            last_sync: from_ms(r.1),
            last_error: r.2,
            items_found: r.3 as u32,
            items_added: r.4 as u32,
        })
        .collect())
}

async fn last_sync_ms(db: &Db, list_id: i64) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT last_sync FROM import_list_status WHERE list_id = ?")
        .bind(list_id)
        .fetch_optional(db.pool())
        .await
}

async fn record_status(db: &Db, o: &ListSyncOutcome) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO import_list_status (list_id, last_sync, last_error, items_found, items_added)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (list_id) DO UPDATE SET last_sync = excluded.last_sync, last_error = excluded.last_error,
           items_found = excluded.items_found, items_added = excluded.items_added",
    )
    .bind(o.list_id)
    .bind(now_ms())
    .bind(&o.error)
    .bind(i64::from(o.found))
    .bind(i64::from(o.added))
    .execute(db.pool())
    .await?;
    Ok(())
}

/// TMDB ids (and series TVDB ids) already in the library.
#[derive(Default)]
struct InLibrary {
    movies: HashSet<u32>,
    series: HashSet<u32>,
    series_tvdb: HashSet<u32>,
}

async fn in_library(db: &Db) -> sqlx::Result<InLibrary> {
    let movies: Vec<i64> = sqlx::query_scalar("SELECT tmdb_id FROM movies")
        .fetch_all(db.pool())
        .await?;
    let series: Vec<(i64, Option<i64>)> = sqlx::query_as("SELECT tmdb_id, tvdb_id FROM series")
        .fetch_all(db.pool())
        .await?;
    Ok(InLibrary {
        movies: movies.into_iter().map(|i| i as u32).collect(),
        series: series.iter().map(|(t, _)| *t as u32).collect(),
        series_tvdb: series.iter().filter_map(|(_, t)| t.map(|t| t as u32)).collect(),
    })
}

// ---------------------------------------------------------------- sync

impl IntegrationsService {
    /// Resolves a list item to a TMDB id (ids first, then a title + year search).
    async fn resolve_tmdb(&self, item: &ListItem) -> Option<u32> {
        if let Some(id) = item.tmdb_id.and_then(|i| u32::try_from(i).ok()) {
            return Some(id);
        }
        let first = |hits: Vec<reelix_metadata::SearchHit>| hits.into_iter().find_map(|h| h.tmdb_id);
        if item.kind == MediaKind::Series
            && let Some(tvdb) = item.tvdb_id
            && let Ok(hits) = self
                .metadata
                .find(ExternalSource::Tvdb, &tvdb.to_string(), item.kind)
                .await
            && let Some(id) = first(hits)
        {
            return Some(id);
        }
        if let Some(imdb) = &item.imdb_id
            && let Ok(hits) = self.metadata.find(ExternalSource::Imdb, imdb, item.kind).await
            && let Some(id) = first(hits)
        {
            return Some(id);
        }
        let title = item.title.trim();
        if title.is_empty() {
            return None;
        }
        let hits = self.metadata.search(title, item.kind, item.year).await.ok()?;
        let norm = |s: &str| reelix_parser::clean_title(s);
        hits.into_iter()
            .find(|h| {
                norm(&h.title) == norm(title)
                    && match (item.year, h.year) {
                        (Some(a), Some(b)) => (a - b).abs() <= 1,
                        _ => true,
                    }
            })
            .and_then(|h| h.tmdb_id)
    }

    /// Syncs every enabled list that is due (or forced / selected by the payload).
    pub async fn sync_import_lists(&self, payload: &SyncPayload) -> anyhow::Result<Vec<ListSyncOutcome>> {
        let lists: Vec<_> = (self.sources.import_lists)()
            .into_iter()
            .filter(|(row, _)| row.enabled)
            .filter(|(row, _)| payload.list_ids.is_empty() || payload.list_ids.contains(&row.id))
            .collect();
        if lists.is_empty() {
            return Ok(Vec::new());
        }
        let now = now_ms();
        let mut outcomes = Vec::new();
        let mut excluded = excluded_set(&self.db).await?;
        let mut existing = in_library(&self.db).await?;
        let roots = self.library.root_folders().await.unwrap_or_default();
        let default_profile = self.db.quality_profiles().list().await?.first().map(|p| p.id);
        for (row, list) in lists {
            if !payload.force
                && let Some(last) = last_sync_ms(&self.db, row.id).await?
            {
                let interval = reelix_lists::sync_interval(&serde_json::Value::Object(row.settings.clone()));
                if now - last < interval.as_millis() as i64 {
                    continue;
                }
            }
            let opts = ListOptions::from_row(&row);
            let mut o = ListSyncOutcome {
                list_id: row.id,
                ..Default::default()
            };
            match list.fetch().await {
                Err(e) => {
                    tracing::warn!(list = %row.name, error = %e, "import list fetch failed");
                    o.error = Some(e.to_string());
                }
                Ok(items) => {
                    o.found = items.len() as u32;
                    if opts.enabled_auto_add {
                        self.add_items(
                            &row,
                            &opts,
                            items,
                            &roots,
                            default_profile,
                            &mut excluded,
                            &mut existing,
                            &mut o,
                        )
                        .await;
                    }
                }
            }
            record_status(&self.db, &o).await?;
            tracing::info!(
                list = %row.name, found = o.found, added = o.added, existing = o.existing,
                excluded = o.excluded, unresolved = o.unresolved, error = ?o.error, "import list synced"
            );
            self.events.publish(DomainEvent::EntityChanged {
                entity: "import_list".into(),
                action: "synced".into(),
                id: row.id,
                user_id: None,
            });
            outcomes.push(o);
        }
        Ok(outcomes)
    }

    #[allow(clippy::too_many_arguments)]
    async fn add_items(
        &self,
        row: &ProviderRow,
        opts: &ListOptions,
        items: Vec<ListItem>,
        roots: &[RootFolderDto],
        default_profile: Option<i64>,
        excluded: &mut HashSet<(MediaKind, u32)>,
        existing: &mut InLibrary,
        o: &mut ListSyncOutcome,
    ) {
        let profile = opts.quality_profile_id.or(default_profile);
        let root_for = |kind: MediaKind| {
            opts.root_folder_id
                .and_then(|id| roots.iter().find(|r| r.id == id && r.media_kind == kind))
                .or_else(|| roots.iter().find(|r| r.media_kind == kind))
                .map(|r| r.id)
        };
        let mut warned: HashSet<MediaKind> = HashSet::new();
        for item in items {
            if item.kind == MediaKind::Series
                && let Some(tvdb) = item.tvdb_id.and_then(|t| u32::try_from(t).ok())
                && existing.series_tvdb.contains(&tvdb)
            {
                o.existing += 1;
                continue;
            }
            let Some(tmdb) = self.resolve_tmdb(&item).await else {
                tracing::debug!(list = %row.name, title = %item.title, "import list item not found on TMDB");
                o.unresolved += 1;
                continue;
            };
            if excluded.contains(&(item.kind, tmdb)) {
                o.excluded += 1;
                continue;
            }
            let known = match item.kind {
                MediaKind::Movie => &mut existing.movies,
                MediaKind::Series => &mut existing.series,
            };
            if known.contains(&tmdb) {
                o.existing += 1;
                continue;
            }
            let (Some(profile), Some(root)) = (profile, root_for(item.kind)) else {
                if warned.insert(item.kind) {
                    let msg = format!(
                        "cannot add {}: no quality profile or {} root folder configured",
                        kind_str(item.kind),
                        kind_str(item.kind)
                    );
                    tracing::warn!(list = %row.name, "{msg}");
                    o.error.get_or_insert(msg);
                }
                continue;
            };
            let result = match item.kind {
                MediaKind::Movie => self
                    .library
                    .add_movie(AddMovieRequest {
                        tmdb_id: tmdb,
                        quality_profile_id: profile,
                        root_folder_id: root,
                        monitor: opts.monitor,
                        minimum_availability: opts.minimum_availability,
                        tags: opts.tags.clone(),
                        search_on_add: opts.search_on_add,
                        path: None,
                        added_by_request_id: None,
                    })
                    .await
                    .map(|m| m.title),
                MediaKind::Series => self
                    .library
                    .add_series(AddSeriesRequest {
                        tmdb_id: Some(tmdb),
                        tvdb_id: None,
                        quality_profile_id: profile,
                        root_folder_id: root,
                        monitor: opts.series_monitor,
                        seasons: None,
                        series_type: opts.series_type,
                        season_folder: opts.season_folder,
                        monitor_new_items: NewItemMonitor::All,
                        tags: opts.tags.clone(),
                        search_on_add: opts.search_on_add,
                        search_cutoff_unmet: false,
                        path: None,
                        added_by_request_id: None,
                    })
                    .await
                    .map(|s| s.title),
            };
            known.insert(tmdb);
            match result {
                Ok(title) => {
                    o.added += 1;
                    tracing::info!(list = %row.name, %title, tmdb, "added from import list");
                }
                Err(LibraryError::Conflict(_)) => o.existing += 1,
                Err(e) => {
                    tracing::warn!(list = %row.name, title = %item.title, tmdb, error = %e, "import list add failed");
                }
            }
        }
    }
}
