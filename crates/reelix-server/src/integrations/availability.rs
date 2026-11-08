//! `media_server_availability` (every 6 h): records which library movies/series exist on each
//! media server (matched by TMDB, then TVDB/IMDb ids).
//!
//! Only movie/TV servers take part ([`tracks_availability`]); book and music servers
//! (Audiobookshelf, Kavita, Calibre-Web, Navidrome) are refreshed after imports instead
//! ([`servers_for`], used by [`super::bridge::LibraryRefresher`]).

use std::collections::HashMap;
use std::sync::Arc;

use reelix_api_types::integrations::{MediaServerAvailabilityDto, MediaServerSyncDto};
use reelix_core::{ContentKind, MediaKind};
use reelix_db::{Db, from_ms, now_ms};
use reelix_mediaserver::{MediaServer, MsItem};
use reelix_providers::ProviderRow;

/// Whether a server's library is matched against movies/series (Jellyfin, Emby, Plex).
pub fn tracks_availability(server: &dyn MediaServer) -> bool {
    server
        .content_kinds()
        .iter()
        .any(|k| matches!(k, ContentKind::Movie | ContentKind::Series))
}

/// Enabled servers that serve `kind` (the ones to refresh after importing it).
pub fn servers_for(
    servers: Vec<(ProviderRow, Arc<dyn MediaServer>)>,
    kind: ContentKind,
) -> Vec<(ProviderRow, Arc<dyn MediaServer>)> {
    servers
        .into_iter()
        .filter(|(row, server)| row.enabled && server.content_kinds().contains(&kind))
        .collect()
}

fn kind_str(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
    }
}

/// Library id lookups by external id.
#[derive(Default)]
struct Index {
    tmdb: HashMap<(MediaKind, i64), i64>,
    tvdb: HashMap<i64, i64>,
    imdb: HashMap<(MediaKind, String), i64>,
}

async fn index(db: &Db) -> sqlx::Result<Index> {
    let mut ix = Index::default();
    let movies: Vec<(i64, i64, Option<String>)> = sqlx::query_as("SELECT id, tmdb_id, imdb_id FROM movies")
        .fetch_all(db.pool())
        .await?;
    for (id, tmdb, imdb) in movies {
        ix.tmdb.insert((MediaKind::Movie, tmdb), id);
        if let Some(i) = imdb {
            ix.imdb.insert((MediaKind::Movie, i), id);
        }
    }
    let series: Vec<(i64, i64, Option<i64>, Option<String>)> =
        sqlx::query_as("SELECT id, tmdb_id, tvdb_id, imdb_id FROM series")
            .fetch_all(db.pool())
            .await?;
    for (id, tmdb, tvdb, imdb) in series {
        ix.tmdb.insert((MediaKind::Series, tmdb), id);
        if let Some(t) = tvdb {
            ix.tvdb.insert(t, id);
        }
        if let Some(i) = imdb {
            ix.imdb.insert((MediaKind::Series, i), id);
        }
    }
    Ok(ix)
}

impl Index {
    fn find(&self, item: &MsItem) -> Option<i64> {
        item.tmdb_id
            .and_then(|t| self.tmdb.get(&(item.kind, t)))
            .or_else(|| {
                (item.kind == MediaKind::Series)
                    .then(|| item.tvdb_id.and_then(|t| self.tvdb.get(&t)))
                    .flatten()
            })
            .or_else(|| {
                item.imdb_id
                    .as_ref()
                    .and_then(|i| self.imdb.get(&(item.kind, i.clone())))
            })
            .copied()
    }
}

/// One server's fetched items (or the error that prevented fetching them).
pub struct ServerItems {
    pub server_id: i64,
    pub items: Result<Vec<MsItem>, String>,
}

/// Rows per INSERT: 5 values each, under the 999 values an older SQLite allows in one statement.
const ROWS_PER_INSERT: usize = 150;

/// Replaces the stored availability of each server with the matched items; forgets servers
/// that are no longer configured.
pub async fn record(db: &Db, results: Vec<ServerItems>) -> sqlx::Result<()> {
    let ix = index(db).await?;
    let now = now_ms();
    let mut tx = db.pool().begin().await?;
    let keep: Vec<i64> = results.iter().map(|r| r.server_id).collect();
    let stale: Vec<i64> = sqlx::query_scalar("SELECT server_id FROM media_server_sync")
        .fetch_all(&mut *tx)
        .await?;
    for id in stale.into_iter().filter(|id| !keep.contains(id)) {
        sqlx::query("DELETE FROM media_server_sync WHERE server_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM media_server_availability WHERE server_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    for r in results {
        match r.items {
            Err(e) => {
                sqlx::query(
                    "INSERT INTO media_server_sync (server_id, synced_at, items_total, last_error) VALUES (?, ?, 0, ?)
                     ON CONFLICT (server_id) DO UPDATE SET last_error = excluded.last_error",
                )
                .bind(r.server_id)
                .bind(now)
                .bind(e)
                .execute(&mut *tx)
                .await?;
            }
            Ok(items) => {
                sqlx::query("DELETE FROM media_server_availability WHERE server_id = ?")
                    .bind(r.server_id)
                    .execute(&mut *tx)
                    .await?;
                let matched: Vec<(&MsItem, i64)> = items
                    .iter()
                    .filter_map(|i| ix.find(i).map(|id| (i, id)))
                    .collect();
                // Many rows per statement: a big library is written in a short transaction, so
                // the sync does not hold the write lock for long (RFX-71).
                for chunk in matched.chunks(ROWS_PER_INSERT) {
                    let mut q = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                        "INSERT OR REPLACE INTO media_server_availability \
                         (server_id, media_kind, media_id, item_id, synced_at) ",
                    );
                    q.push_values(chunk, |mut row, (item, media_id)| {
                        row.push_bind(r.server_id)
                            .push_bind(kind_str(item.kind))
                            .push_bind(*media_id)
                            .push_bind(item.id.as_str())
                            .push_bind(now);
                    });
                    q.build().execute(&mut *tx).await?;
                }
                sqlx::query(
                    "INSERT INTO media_server_sync (server_id, synced_at, items_total, last_error) VALUES (?, ?, ?, NULL)
                     ON CONFLICT (server_id) DO UPDATE SET synced_at = excluded.synced_at,
                       items_total = excluded.items_total, last_error = NULL",
                )
                .bind(r.server_id)
                .bind(now)
                .bind(items.len() as i64)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await
}

/// Summary for `GET /mediaservers/availability`; `names` maps configured server ids to names.
pub async fn summary(db: &Db, names: &[(i64, String)]) -> sqlx::Result<MediaServerAvailabilityDto> {
    let count =
        |sql: &'static str| async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(db.pool()).await };
    let movies_total = count("SELECT COUNT(*) FROM movies").await?;
    let series_total = count("SELECT COUNT(*) FROM series").await?;
    let movies_available = count(
        "SELECT COUNT(DISTINCT media_id) FROM media_server_availability a JOIN movies m ON m.id = a.media_id
         WHERE a.media_kind = 'movie'",
    )
    .await?;
    let series_available = count(
        "SELECT COUNT(DISTINCT media_id) FROM media_server_availability a JOIN series s ON s.id = a.media_id
         WHERE a.media_kind = 'series'",
    )
    .await?;
    let syncs: Vec<(i64, i64, i64, Option<String>)> =
        sqlx::query_as("SELECT server_id, synced_at, items_total, last_error FROM media_server_sync")
            .fetch_all(db.pool())
            .await?;
    let matched: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT server_id, media_kind, COUNT(*) FROM media_server_availability GROUP BY server_id, media_kind",
    )
    .fetch_all(db.pool())
    .await?;
    let servers = names
        .iter()
        .map(|(id, name)| {
            let sync = syncs.iter().find(|s| s.0 == *id);
            let m = |k: &str| {
                matched
                    .iter()
                    .find(|(s, kind, _)| s == id && kind == k)
                    .map_or(0, |r| r.2 as u32)
            };
            MediaServerSyncDto {
                server_id: *id,
                name: name.clone(),
                synced_at: sync.map(|s| from_ms(s.1)),
                items_total: sync.map_or(0, |s| s.2 as u32),
                movies_matched: m("movie"),
                series_matched: m("series"),
                last_error: sync.and_then(|s| s.3.clone()),
            }
        })
        .collect();
    Ok(MediaServerAvailabilityDto {
        movies_total: movies_total as u32,
        movies_available: movies_available as u32,
        series_total: series_total as u32,
        series_available: series_available as u32,
        servers,
    })
}

/// Whether a library item is on any media server (for other modules).
pub async fn is_available(db: &Db, kind: MediaKind, media_id: i64) -> sqlx::Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_server_availability WHERE media_kind = ? AND media_id = ?",
    )
    .bind(kind_str(kind))
    .bind(media_id)
    .fetch_one(db.pool())
    .await?;
    Ok(n > 0)
}
