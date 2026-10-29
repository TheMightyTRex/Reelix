//! Notification bridge: turns library `entity.changed` events into notifications
//! (`MediaAdded`, `MediaDeleted`, `FileDeleted`, `Rename`).
//!
//! Deletions are announced after the rows are gone, so the bridge keeps a small in-memory
//! snapshot of every movie/series (title, ids, path, files) loaded straight from the library
//! tables and refreshed on change events. Grab/Import notifications are sent by the pipeline
//! itself and never by the bridge. File deletions immediately followed by a new file for the
//! same media (an upgrade import) are suppressed: the pipeline's `Import` event already lists
//! the replaced files.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use reelix_api_types::library::ImagesDto;
use reelix_core::{DomainEvent, MediaKind};
use reelix_db::Db;
use reelix_notify::{
    DeleteReason, EpisodeSummary, FileSummary, MediaSummary, NotificationEvent, RenamedFile,
};
use tokio::sync::broadcast;
use tokio::time::Instant;

/// How long a file deletion waits for a replacing file before it is announced.
pub const UPGRADE_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub struct CachedFile {
    pub relative_path: String,
    pub summary: FileSummary,
    pub episodes: Vec<EpisodeSummary>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaEntry {
    pub summary: MediaSummary,
    pub path: String,
    pub files: HashMap<i64, CachedFile>,
}

/// Snapshot of the library used to describe deleted media and files.
#[derive(Debug, Default)]
pub struct MediaCache {
    media: HashMap<(MediaKind, i64), MediaEntry>,
    files: HashMap<(MediaKind, i64), i64>,
}

impl MediaCache {
    fn insert(&mut self, kind: MediaKind, id: i64, entry: Option<MediaEntry>) -> Option<MediaEntry> {
        let old = self.media.remove(&(kind, id));
        if let Some(old) = &old {
            for fid in old.files.keys() {
                self.files.remove(&(kind, *fid));
            }
        }
        if let Some(e) = entry {
            for fid in e.files.keys() {
                self.files.insert((kind, *fid), id);
            }
            self.media.insert((kind, id), e);
        }
        old
    }

    pub fn get(&self, kind: MediaKind, id: i64) -> Option<&MediaEntry> {
        self.media.get(&(kind, id))
    }

    pub fn len(&self) -> usize {
        self.media.len()
    }

    pub fn is_empty(&self) -> bool {
        self.media.is_empty()
    }
}

fn kind_table(kind: MediaKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        MediaKind::Movie => ("movies", "movie_files", "movie_id"),
        MediaKind::Series => ("series", "episode_files", "series_id"),
    }
}

type MediaRow = (
    i64,
    String,
    Option<i64>,
    i64,
    Option<String>,
    Option<i64>,
    String,
    Option<String>,
    String,
);
type FileRow = (i64, i64, String, i64, String, Option<String>, Option<String>);
type EpisodeRow = (i64, i64, i64, Option<i64>, Option<String>, Option<String>);

fn quality_name(json: &str) -> Option<String> {
    serde_json::from_str::<reelix_parser::QualityModel>(json)
        .ok()
        .map(|q| q.quality.name().to_owned())
}

/// Loads media entries (all, or one id) of `kind` from the library tables.
pub async fn load(db: &Db, kind: MediaKind, id: Option<i64>) -> sqlx::Result<Vec<(i64, MediaEntry)>> {
    let (table, files_table, owner) = kind_table(kind);
    let tvdb = if kind == MediaKind::Series {
        "tvdb_id"
    } else {
        "NULL"
    };
    let filter = |col: &str| match id {
        Some(_) => format!(" WHERE {col} = ?1"),
        None => String::new(),
    };
    let sql = format!(
        "SELECT id, title, year, tmdb_id, imdb_id, {tvdb}, images, overview, path FROM {table}{}",
        filter("id")
    );
    let q = sqlx::query_as::<_, MediaRow>(&sql);
    let media = match id {
        Some(i) => q.bind(i),
        None => q,
    }
    .fetch_all(db.pool())
    .await?;
    let sql = format!(
        "SELECT id, {owner}, relative_path, size, quality, release_group, scene_name FROM {files_table}{}",
        filter(owner)
    );
    let q = sqlx::query_as::<_, FileRow>(&sql);
    let files = match id {
        Some(i) => q.bind(i),
        None => q,
    }
    .fetch_all(db.pool())
    .await?;
    let episodes: Vec<EpisodeRow> = if kind == MediaKind::Series {
        let sql = format!(
            "SELECT episode_file_id, season_number, episode_number, absolute_number, title, air_date FROM episodes
             WHERE episode_file_id IS NOT NULL{}
             ORDER BY season_number, episode_number",
            if id.is_some() { " AND series_id = ?1" } else { "" }
        );
        let q = sqlx::query_as::<_, EpisodeRow>(&sql);
        match id {
            Some(i) => q.bind(i),
            None => q,
        }
        .fetch_all(db.pool())
        .await?
    } else {
        Vec::new()
    };
    let mut eps_by_file: HashMap<i64, Vec<EpisodeSummary>> = HashMap::new();
    for (fid, season, episode, absolute, title, air) in episodes {
        eps_by_file.entry(fid).or_default().push(EpisodeSummary {
            season: season as u32,
            episode: episode as u32,
            absolute: absolute.map(|a| a as u32),
            title,
            air_date: air,
        });
    }
    let mut out: HashMap<i64, MediaEntry> = media
        .into_iter()
        .map(|(id, title, year, tmdb, imdb, tvdb, images, overview, path)| {
            let images: ImagesDto = serde_json::from_str(&images).unwrap_or_default();
            let summary = MediaSummary {
                id: Some(id),
                year: year.map(|y| y as i32),
                tmdb_id: Some(tmdb),
                imdb_id: imdb,
                tvdb_id: tvdb,
                poster_url: images.poster,
                overview,
                ..MediaSummary::new(kind, title)
            };
            (
                id,
                MediaEntry {
                    summary,
                    path,
                    files: HashMap::new(),
                },
            )
        })
        .collect();
    for (fid, owner_id, rel, size, quality, group, scene) in files {
        let Some(entry) = out.get_mut(&owner_id) else {
            continue;
        };
        let full = Path::new(&entry.path).join(&rel).to_string_lossy().into_owned();
        entry.files.insert(
            fid,
            CachedFile {
                relative_path: rel,
                summary: FileSummary {
                    path: full,
                    size_bytes: u64::try_from(size).ok(),
                    quality: quality_name(&quality),
                    release_group: group,
                    scene_name: scene,
                },
                episodes: eps_by_file.remove(&fid).unwrap_or_default(),
            },
        );
    }
    Ok(out.into_iter().collect())
}

async fn file_owner(db: &Db, kind: MediaKind, file_id: i64) -> sqlx::Result<Option<i64>> {
    let (_, files_table, owner) = kind_table(kind);
    sqlx::query_scalar(&format!("SELECT {owner} FROM {files_table} WHERE id = ?"))
        .bind(file_id)
        .fetch_optional(db.pool())
        .await
}

/// A file deletion waiting for [`UPGRADE_GRACE`].
#[derive(Debug)]
struct PendingDelete {
    kind: MediaKind,
    media_id: i64,
    media: MediaSummary,
    file: CachedFile,
    due: Instant,
}

/// Turns library change events into notification events.
pub struct Bridge {
    db: Db,
    cache: MediaCache,
    pending: Vec<PendingDelete>,
    grace: Duration,
}

fn media_kind(entity: &str) -> Option<(MediaKind, bool)> {
    match entity {
        "movie" => Some((MediaKind::Movie, false)),
        "series" => Some((MediaKind::Series, false)),
        "movie_file" => Some((MediaKind::Movie, true)),
        "episode_file" => Some((MediaKind::Series, true)),
        _ => None,
    }
}

impl Bridge {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            cache: MediaCache::default(),
            pending: Vec::new(),
            grace: UPGRADE_GRACE,
        }
    }

    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    pub fn cache(&self) -> &MediaCache {
        &self.cache
    }

    /// Re-reads every movie and series.
    pub async fn reload_all(&mut self) -> sqlx::Result<()> {
        let mut cache = MediaCache::default();
        for kind in [MediaKind::Movie, MediaKind::Series] {
            for (id, e) in load(&self.db, kind, None).await? {
                cache.insert(kind, id, Some(e));
            }
        }
        self.cache = cache;
        Ok(())
    }

    async fn reload(&mut self, kind: MediaKind, id: i64) -> Option<MediaEntry> {
        match load(&self.db, kind, Some(id)).await {
            Ok(mut v) => {
                let entry = v.pop().map(|(_, e)| e);
                self.cache.insert(kind, id, entry)
            }
            Err(e) => {
                tracing::warn!(error = %e, "notification bridge: cannot load media");
                self.cache.get(kind, id).cloned()
            }
        }
    }

    /// Handles one event; returns the notifications to send now.
    pub async fn handle(&mut self, event: &DomainEvent) -> Vec<NotificationEvent> {
        let DomainEvent::EntityChanged {
            entity, action, id, ..
        } = event
        else {
            return Vec::new();
        };
        let Some((kind, is_file)) = media_kind(entity) else {
            return Vec::new();
        };
        let id = *id;
        let mut out = Vec::new();
        if is_file {
            match action.as_str() {
                "deleted" => {
                    let Some(media_id) = self.cache.files.get(&(kind, id)).copied() else {
                        return out;
                    };
                    let Some(entry) = self.cache.media.get_mut(&(kind, media_id)) else {
                        return out;
                    };
                    let Some(file) = entry.files.remove(&id) else {
                        return out;
                    };
                    self.cache.files.remove(&(kind, id));
                    self.pending.push(PendingDelete {
                        kind,
                        media_id,
                        media: entry.summary.clone(),
                        file,
                        due: Instant::now() + self.grace,
                    });
                }
                "created" => {
                    if let Ok(Some(owner)) = file_owner(&self.db, kind, id).await {
                        // A replacement file: the deletion was part of an upgrade import.
                        self.pending.retain(|p| !(p.kind == kind && p.media_id == owner));
                        self.reload(kind, owner).await;
                    }
                }
                _ => {}
            }
            return out;
        }
        match action.as_str() {
            "created" => {
                self.reload(kind, id).await;
                if let Some(e) = self.cache.get(kind, id) {
                    out.push(NotificationEvent::MediaAdded {
                        media: e.summary.clone(),
                    });
                }
            }
            "deleted" => {
                self.pending.retain(|p| !(p.kind == kind && p.media_id == id));
                if let Some(old) = self.cache.insert(kind, id, None) {
                    let files_deleted = !old.files.is_empty() && !Path::new(&old.path).exists();
                    out.push(NotificationEvent::MediaDeleted {
                        media: old.summary,
                        files_deleted,
                    });
                }
            }
            "renamed" => {
                let old = self.cache.get(kind, id).cloned();
                self.reload(kind, id).await;
                if let (Some(old), Some(new)) = (old, self.cache.get(kind, id)) {
                    let mut files: Vec<RenamedFile> = new
                        .files
                        .iter()
                        .filter_map(|(fid, f)| {
                            let prev = old.files.get(fid)?;
                            (prev.relative_path != f.relative_path).then(|| RenamedFile {
                                previous_path: prev.summary.path.clone(),
                                new_path: f.summary.path.clone(),
                            })
                        })
                        .collect();
                    files.sort_by(|a, b| a.new_path.cmp(&b.new_path));
                    if !files.is_empty() {
                        out.push(NotificationEvent::Rename {
                            media: new.summary.clone(),
                            files,
                        });
                    }
                }
            }
            _ => {
                self.reload(kind, id).await;
            }
        }
        out
    }

    /// File deletions whose grace period has passed.
    pub fn flush(&mut self, now: Instant) -> Vec<NotificationEvent> {
        let (due, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| p.due <= now);
        self.pending = keep;
        due.into_iter()
            .map(|p| {
                let mut media = p.media;
                media.episodes = p.file.episodes;
                NotificationEvent::FileDeleted {
                    media,
                    file: p.file.summary,
                    reason: DeleteReason::Manual,
                }
            })
            .collect()
    }

    fn next_due(&self) -> Option<Instant> {
        self.pending.iter().map(|p| p.due).min()
    }

    /// Runs until the event bus closes: loads the snapshot, then handles events and sends
    /// the resulting notifications through `notify`.
    pub async fn run<F, Fut>(mut self, mut rx: broadcast::Receiver<DomainEvent>, notify: F)
    where
        F: Fn(NotificationEvent) -> Fut + Send,
        Fut: std::future::Future<Output = ()> + Send,
    {
        if let Err(e) = self.reload_all().await {
            tracing::warn!(error = %e, "notification bridge: initial load failed");
        }
        loop {
            let next = self.next_due();
            let recv = async {
                match next {
                    Some(due) => tokio::time::timeout_at(due, rx.recv()).await.ok(),
                    None => Some(rx.recv().await),
                }
            };
            match recv.await {
                None => {}
                Some(Ok(ev)) => {
                    for n in self.handle(&ev).await {
                        notify(n).await;
                    }
                }
                Some(Err(broadcast::error::RecvError::Lagged(n))) => {
                    tracing::warn!(missed = n, "notification bridge lagged; reloading snapshot");
                    if let Err(e) = self.reload_all().await {
                        tracing::warn!(error = %e, "notification bridge: reload failed");
                    }
                }
                Some(Err(broadcast::error::RecvError::Closed)) => break,
            }
            for n in self.flush(Instant::now()) {
                notify(n).await;
            }
        }
    }
}
