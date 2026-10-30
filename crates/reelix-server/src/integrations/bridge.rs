//! Notification bridge: turns library `entity.changed` events into notifications
//! (`MediaAdded`, `MediaDeleted`, `FileDeleted`, `Rename`).
//!
//! Deletions are announced after the rows are gone, so the bridge keeps a small in-memory
//! snapshot of every movie/series (title, ids, path, files) loaded straight from the library
//! tables and refreshed on change events. Grab/Import notifications are sent by the pipeline
//! itself and never by the bridge. File deletions immediately followed by a new file for the
//! same media (an upgrade import) are suppressed: the pipeline's `Import` event already lists
//! the replaced files.
//!
//! [`LibraryRefresher`] is the media-server side for books and music: after a book file or
//! album import it asks the servers serving that content kind (Audiobookshelf, Kavita,
//! Calibre-Web, Navidrome, or any server declaring the kind) to rescan the imported path.
//! Movie/TV refreshes stay with the pipeline's import step.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::library::ImagesDto;
use reelix_core::{ContentKind, DomainEvent, MediaKind};
use reelix_db::Db;
use reelix_mediaserver::MediaServer;
use reelix_notify::{
    DeleteReason, EpisodeSummary, FileSummary, MediaSummary, NotificationEvent, RenamedFile,
};
use reelix_providers::ProviderRow;
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

// ---------------------------------------------------------------- book/music refresh

/// Configured media servers (the provider registry in production, fakes in tests).
pub type MediaServersFn = dyn Fn() -> Vec<(ProviderRow, Arc<dyn MediaServer>)> + Send + Sync;

/// How long one server may take to accept a refresh.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);

/// A finished book or album import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedContent {
    pub kind: ContentKind,
    /// Imported file or folder (Reelix-side path; adapters apply their path mappings).
    pub path: PathBuf,
}

/// Refreshes book/music servers after imports, driven by `entity.changed` events:
/// `book_file created` (book import) and `album updated` / `track_file created` (album import;
/// only when track files changed since the last refresh, so metadata edits do not rescan).
pub struct LibraryRefresher {
    db: Db,
    servers: Arc<MediaServersFn>,
    timeout: Duration,
    /// Highest `track_files.updated_at` already announced.
    music_seen: i64,
}

type BookFileRow = (String, String, Option<String>, Option<String>);
type TrackFileRow = (String, String, i64);

impl LibraryRefresher {
    pub fn new(db: Db, servers: Arc<MediaServersFn>) -> Self {
        Self {
            db,
            servers,
            timeout: REFRESH_TIMEOUT,
            music_seen: i64::MAX,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Marks every existing track file as already announced (call before handling events).
    pub async fn init(&mut self) -> sqlx::Result<()> {
        self.music_seen = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(updated_at) FROM track_files")
            .fetch_one(self.db.pool())
            .await?
            .unwrap_or(0);
        Ok(())
    }

    /// The import an event announces, if any.
    pub async fn imported(&mut self, event: &DomainEvent) -> sqlx::Result<Option<ImportedContent>> {
        let DomainEvent::EntityChanged {
            entity, action, id, ..
        } = event
        else {
            return Ok(None);
        };
        match (entity.as_str(), action.as_str()) {
            ("book_file", "created" | "imported") => self.book_file(*id).await,
            ("album", "updated" | "imported") => self.album(*id, None).await,
            ("track_file" | "trackfile", "created" | "imported") => self.album(0, Some(*id)).await,
            _ => Ok(None),
        }
    }

    async fn book_file(&self, file_id: i64) -> sqlx::Result<Option<ImportedContent>> {
        let row: Option<BookFileRow> = sqlx::query_as(
            "SELECT f.format, f.relative_path, a.path, a.audio_path FROM book_files f
             JOIN books b ON b.id = f.book_id JOIN authors a ON a.id = b.author_id WHERE f.id = ?",
        )
        .bind(file_id)
        .fetch_optional(self.db.pool())
        .await?;
        let Some((format, rel, path, audio_path)) = row else {
            return Ok(None);
        };
        let (kind, base) = match format.as_str() {
            "audiobook" => (ContentKind::Audiobook, audio_path.or(path)),
            _ => (ContentKind::Ebook, path.or(audio_path)),
        };
        Ok(base.map(|b| ImportedContent {
            kind,
            path: Path::new(&b).join(rel),
        }))
    }

    /// Track files of `album_id` (or the single `file_id`) changed since the last announcement.
    async fn album(&mut self, album_id: i64, file_id: Option<i64>) -> sqlx::Result<Option<ImportedContent>> {
        let (filter, key) = match file_id {
            Some(f) => ("t.id = ?", f),
            None => ("t.album_id = ?", album_id),
        };
        let rows: Vec<TrackFileRow> = sqlx::query_as(&format!(
            "SELECT t.relative_path, ar.path, t.updated_at FROM track_files t
             JOIN albums al ON al.id = t.album_id JOIN artists ar ON ar.id = al.artist_id
             WHERE {filter} AND t.updated_at > ? ORDER BY t.updated_at DESC"
        ))
        .bind(key)
        .bind(self.music_seen)
        .fetch_all(self.db.pool())
        .await?;
        let Some((rel, artist_path, newest)) = rows.into_iter().next() else {
            return Ok(None);
        };
        self.music_seen = self.music_seen.max(newest);
        // The album folder: the file's parent (relative paths may nest disc folders).
        let file = Path::new(&artist_path).join(&rel);
        let rel_parent = Path::new(&rel).parent().filter(|p| !p.as_os_str().is_empty());
        let path = match rel_parent.and_then(|p| p.components().next()) {
            Some(first) => Path::new(&artist_path).join(first),
            None => file.parent().map_or(file.clone(), Path::to_path_buf),
        };
        Ok(Some(ImportedContent {
            kind: ContentKind::Music,
            path,
        }))
    }

    /// Asks every enabled server serving `content.kind` to rescan `content.path`; returns the
    /// ids of the servers that accepted.
    pub async fn refresh(servers: &MediaServersFn, timeout: Duration, content: &ImportedContent) -> Vec<i64> {
        let targets = super::availability::servers_for(servers(), content.kind);
        let paths = std::slice::from_ref(&content.path);
        let mut ok = Vec::new();
        for (row, server) in targets {
            match tokio::time::timeout(timeout, server.refresh_library(paths)).await {
                Ok(Ok(())) => {
                    tracing::debug!(server = %row.name, path = %content.path.display(), "media server refresh requested");
                    ok.push(row.id);
                }
                Ok(Err(e)) => {
                    tracing::warn!(server = %row.name, error = %e, "media server refresh failed")
                }
                Err(_) => tracing::warn!(server = %row.name, "media server refresh timed out"),
            }
        }
        ok
    }

    /// Handles one event and waits for the refreshes; returns the refreshed server ids.
    pub async fn handle(&mut self, event: &DomainEvent) -> Vec<i64> {
        match self.imported(event).await {
            Ok(Some(c)) => Self::refresh(self.servers.as_ref(), self.timeout, &c).await,
            Ok(None) => Vec::new(),
            Err(e) => {
                tracing::warn!(error = %e, "media server refresh: cannot load the imported files");
                Vec::new()
            }
        }
    }

    /// Runs until the event bus closes; refreshes run in the background so a slow server
    /// never holds up later events.
    pub async fn run(mut self, mut rx: broadcast::Receiver<DomainEvent>) {
        if let Err(e) = self.init().await {
            tracing::warn!(error = %e, "media server refresh: initial load failed");
            self.music_seen = reelix_db::now_ms();
        }
        loop {
            match rx.recv().await {
                Ok(ev) => match self.imported(&ev).await {
                    Ok(Some(c)) => {
                        let (servers, timeout) = (self.servers.clone(), self.timeout);
                        tokio::spawn(async move {
                            Self::refresh(servers.as_ref(), timeout, &c).await;
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "media server refresh: cannot load the imported files")
                    }
                },
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        missed = n,
                        "media server refresh lagged; some imports may not be announced"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use reelix_api_types::providers::ProviderKind;
    use reelix_mediaserver::{ExternalUser, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem};

    use super::*;

    /// Records refresh calls; serves the given content kinds.
    struct Recorder {
        kind: MediaServerKind,
        kinds: Vec<ContentKind>,
        calls: Mutex<Vec<Vec<PathBuf>>>,
    }

    impl Recorder {
        fn new(kind: MediaServerKind, kinds: Vec<ContentKind>) -> Arc<Self> {
            Arc::new(Self {
                kind,
                kinds,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<Vec<PathBuf>> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MediaServer for Recorder {
        fn kind(&self) -> MediaServerKind {
            self.kind
        }
        async fn test(&self) -> Result<ServerInfo, MsError> {
            Err(MsError::Unsupported("fake"))
        }
        async fn authenticate(&self, _u: &str, _p: &str) -> Result<ExternalUser, MsError> {
            Err(MsError::Unsupported("fake"))
        }
        async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
            Ok(vec![])
        }
        async fn refresh_library(&self, paths: &[PathBuf]) -> Result<(), MsError> {
            self.calls.lock().unwrap().push(paths.to_vec());
            Ok(())
        }
        async fn library_items(&self) -> Result<Vec<MsItem>, MsError> {
            Ok(vec![])
        }
        async fn watch_history(&self, _u: &str) -> Result<Vec<WatchedItem>, MsError> {
            Ok(vec![])
        }
        fn content_kinds(&self) -> Vec<ContentKind> {
            self.kinds.clone()
        }
    }

    fn row(id: i64, name: &str, enabled: bool) -> ProviderRow {
        ProviderRow {
            id,
            kind: ProviderKind::MediaServer,
            implementation: "fake".into(),
            name: name.into(),
            enabled,
            priority: 25,
            tags: vec![],
            settings: serde_json::Map::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn changed(entity: &str, action: &str, id: i64) -> DomainEvent {
        DomainEvent::EntityChanged {
            entity: entity.into(),
            action: action.into(),
            id,
            user_id: None,
        }
    }

    struct Env {
        db: Db,
        jellyfin: Arc<Recorder>,
        abs: Arc<Recorder>,
        kavita: Arc<Recorder>,
        navidrome: Arc<Recorder>,
        disabled_kavita: Arc<Recorder>,
        refresher: LibraryRefresher,
    }

    async fn env() -> Env {
        let db = Db::in_memory().await.unwrap();
        let jellyfin = Recorder::new(
            MediaServerKind::Jellyfin,
            vec![ContentKind::Movie, ContentKind::Series],
        );
        let abs = Recorder::new(MediaServerKind::Audiobookshelf, vec![ContentKind::Audiobook]);
        let kavita = Recorder::new(MediaServerKind::Kavita, vec![ContentKind::Ebook]);
        let navidrome = Recorder::new(MediaServerKind::Navidrome, vec![ContentKind::Music]);
        let disabled_kavita = Recorder::new(MediaServerKind::Kavita, vec![ContentKind::Ebook]);
        let list: Vec<(ProviderRow, Arc<dyn MediaServer>)> = vec![
            (row(1, "Jellyfin", true), jellyfin.clone()),
            (row(2, "ABS", true), abs.clone()),
            (row(3, "Kavita", true), kavita.clone()),
            (row(4, "Navidrome", true), navidrome.clone()),
            (row(5, "Kavita (off)", false), disabled_kavita.clone()),
        ];
        let servers: Arc<MediaServersFn> = Arc::new(move || list.clone());
        let refresher = LibraryRefresher::new(db.clone(), servers);
        Env {
            db,
            jellyfin,
            abs,
            kavita,
            navidrome,
            disabled_kavita,
            refresher,
        }
    }

    async fn exec(db: &Db, sql: &str) {
        sqlx::query(sql).execute(db.pool()).await.unwrap();
    }

    async fn seed_books(db: &Db) {
        exec(
            db,
            "INSERT INTO authors (id, openlibrary_id, name, sort_name, clean_name, quality_profile_id,
               path, audio_quality_profile_id, audio_path, created_at, updated_at)
             VALUES (1, 'OL1A', 'Frank Herbert', 'Herbert, Frank', 'frankherbert', 1,
               '/data/books/Frank Herbert', 1, '/data/audiobooks/Frank Herbert', 0, 0)",
        )
        .await;
        exec(
            db,
            "INSERT INTO books (id, author_id, openlibrary_work_id, title, sort_title, clean_title, created_at, updated_at)
             VALUES (1, 1, 'OL1W', 'Dune', 'dune', 'dune', 0, 0)",
        )
        .await;
        exec(
            db,
            "INSERT INTO book_files (id, book_id, format, relative_path, quality, date_added, created_at, updated_at)
             VALUES (10, 1, 'ebook', 'Dune (1965)/Dune.epub', '{}', 0, 0, 0),
                    (11, 1, 'audiobook', 'Dune (1965)', '{}', 0, 0, 0)",
        )
        .await;
    }

    async fn seed_music(db: &Db, updated_at: i64) {
        exec(
            db,
            "INSERT OR IGNORE INTO artists (id, musicbrainz_id, name, sort_name, clean_name, quality_profile_id,
               metadata_profile_id, root_folder_id, path, created_at, updated_at)
             VALUES (1, 'mb-artist', 'Daft Punk', 'Daft Punk', 'daftpunk', 1, 1, 1, '/data/music/Daft Punk', 0, 0)",
        )
        .await;
        exec(
            db,
            "INSERT OR IGNORE INTO albums (id, artist_id, musicbrainz_release_group_id, title, clean_title, created_at, updated_at)
             VALUES (7, 1, 'mb-rg', 'Discovery', 'discovery', 0, 0)",
        )
        .await;
        sqlx::query(
            "INSERT INTO track_files (album_id, relative_path, quality, date_added, created_at, updated_at)
             VALUES (7, ?, '{}', ?, ?, ?)
             ON CONFLICT (album_id, relative_path) DO UPDATE SET updated_at = excluded.updated_at",
        )
        .bind("Discovery (2001)/CD1/01 - One More Time.flac")
        .bind(updated_at)
        .bind(updated_at)
        .bind(updated_at)
        .execute(db.pool())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn book_import_refreshes_book_servers_only() {
        let mut e = env().await;
        seed_books(&e.db).await;
        e.refresher.init().await.unwrap();

        // Ebook import → Kavita (enabled) only.
        let ids = e.refresher.handle(&changed("book_file", "created", 10)).await;
        assert_eq!(ids, [3]);
        assert_eq!(
            e.kavita.calls(),
            [vec![PathBuf::from(
                "/data/books/Frank Herbert/Dune (1965)/Dune.epub"
            )]]
        );
        // Audiobook import → Audiobookshelf, under the author's audiobook folder.
        let ids = e.refresher.handle(&changed("book_file", "created", 11)).await;
        assert_eq!(ids, [2]);
        assert_eq!(
            e.abs.calls(),
            [vec![PathBuf::from("/data/audiobooks/Frank Herbert/Dune (1965)")]]
        );
        // Movie/TV servers, music servers and disabled servers are never asked.
        assert!(e.jellyfin.calls().is_empty());
        assert!(e.navidrome.calls().is_empty());
        assert!(e.disabled_kavita.calls().is_empty());

        // Other book events (metadata edits, deletions, unknown files) do nothing.
        for ev in [
            changed("book", "updated", 1),
            changed("book_file", "deleted", 10),
            changed("book_file", "created", 999),
            changed("movie_file", "created", 10),
        ] {
            assert!(e.refresher.handle(&ev).await.is_empty());
        }
        assert_eq!(e.kavita.calls().len(), 1);
    }

    #[tokio::test]
    async fn album_import_refreshes_music_servers_once_per_change() {
        let mut e = env().await;
        // Files present before startup are not announced.
        seed_music(&e.db, 1_000).await;
        e.refresher.init().await.unwrap();
        assert!(
            e.refresher
                .handle(&changed("album", "updated", 7))
                .await
                .is_empty()
        );

        // An import touches the album's track files, then emits `album updated`.
        seed_music(&e.db, 2_000).await;
        let ids = e.refresher.handle(&changed("album", "updated", 7)).await;
        assert_eq!(ids, [4]);
        assert_eq!(
            e.navidrome.calls(),
            [vec![PathBuf::from("/data/music/Daft Punk/Discovery (2001)")]]
        );
        // A later metadata-only update of the album does not rescan.
        assert!(
            e.refresher
                .handle(&changed("album", "updated", 7))
                .await
                .is_empty()
        );
        assert!(e.jellyfin.calls().is_empty());
        assert!(e.kavita.calls().is_empty());
        assert!(e.abs.calls().is_empty());
    }

    #[test]
    fn availability_only_tracks_video_servers() {
        let jf = Recorder::new(
            MediaServerKind::Jellyfin,
            vec![ContentKind::Movie, ContentKind::Series],
        );
        let nd = Recorder::new(MediaServerKind::Navidrome, vec![ContentKind::Music]);
        assert!(super::super::availability::tracks_availability(jf.as_ref()));
        assert!(!super::super::availability::tracks_availability(nd.as_ref()));
    }
}
