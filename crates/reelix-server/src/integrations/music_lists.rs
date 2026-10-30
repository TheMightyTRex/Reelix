//! Music import lists (Last.fm, ListenBrainz, Spotify): artists and albums are matched to
//! MusicBrainz and added through the music service, skipping `music_list_exclusions` and
//! anything already in the library.
//!
//! Matching: a MusicBrainz id from the list wins (ListenBrainz, Last.fm artists). Otherwise an
//! artist is looked up by name and the best-scored hit whose normalised name equals the list's
//! (case, punctuation and a leading "The" ignored) is used. An album without a release-group id
//! is searched by title and must match the (resolved) artist — by MBID, else by normalised name —
//! and the normalised title; a ±1 year match is preferred. Anything else counts as unresolved.
//! Spotify UPCs are carried on the items but not used for matching yet.

use std::collections::HashSet;

use reelix_api_types::integrations::{
    CreateMusicExclusionRequest, MusicExclusionKind, MusicListExclusionDto,
};
use reelix_api_types::music::{
    AddAlbumRequest, AddArtistRequest, AlbumLookupDto, ArtistLookupDto, ArtistMonitor, NewAlbumMonitor,
};
use reelix_db::{Db, from_ms, now_ms};
use reelix_lists::{MusicItemKind, MusicListItem};
use reelix_music::{MusicError, MusicService};
use reelix_parser::QualityFamily;
use reelix_providers::ProviderRow;
use serde_json::{Map, Value};

use super::lists::ListSyncOutcome;

/// Why an add did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddError {
    /// Already in the library (raced with another add).
    Exists,
    Failed(String),
}

/// Defaults used when a list leaves a target empty.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MusicDefaults {
    pub quality_profile_id: Option<i64>,
    pub metadata_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
}

/// The slice of the music library the list sync needs ([`MusicService`] in production).
#[async_trait::async_trait]
pub trait MusicLibrary: Send + Sync {
    async fn has_artist(&self, mbid: &str) -> Result<bool, String>;
    /// Any album (monitored or not) with this release-group id.
    async fn has_album(&self, release_group_mbid: &str) -> Result<bool, String>;
    async fn search_artists(&self, name: &str) -> Result<Vec<ArtistLookupDto>, String>;
    async fn search_albums(&self, title: &str) -> Result<Vec<AlbumLookupDto>, String>;
    async fn defaults(&self) -> Result<MusicDefaults, String>;
    /// Returns the added artist's name.
    async fn add_artist(&self, req: AddArtistRequest) -> Result<String, AddError>;
    /// Returns the added album's title.
    async fn add_album(&self, req: AddAlbumRequest) -> Result<String, AddError>;
}

fn add_error(e: MusicError) -> AddError {
    match e {
        MusicError::Conflict(_) => AddError::Exists,
        e => AddError::Failed(e.to_string()),
    }
}

#[async_trait::async_trait]
impl MusicLibrary for MusicService {
    async fn has_artist(&self, mbid: &str) -> Result<bool, String> {
        self.artist_by_mbid(mbid)
            .await
            .map(|a| a.is_some())
            .map_err(|e| e.to_string())
    }

    async fn has_album(&self, release_group_mbid: &str) -> Result<bool, String> {
        self.album_by_mbid(release_group_mbid)
            .await
            .map(|a| a.is_some())
            .map_err(|e| e.to_string())
    }

    async fn search_artists(&self, name: &str) -> Result<Vec<ArtistLookupDto>, String> {
        self.lookup_artists(name).await.map_err(|e| e.to_string())
    }

    async fn search_albums(&self, title: &str) -> Result<Vec<AlbumLookupDto>, String> {
        self.lookup_albums(title).await.map_err(|e| e.to_string())
    }

    async fn defaults(&self) -> Result<MusicDefaults, String> {
        let s = |e: MusicError| e.to_string();
        let profiles = self
            .db()
            .quality_profiles()
            .list_family(QualityFamily::Music)
            .await
            .map_err(|e| e.to_string())?;
        Ok(MusicDefaults {
            quality_profile_id: profiles.first().map(|p| p.id),
            metadata_profile_id: self.metadata_profiles().await.map_err(s)?.first().map(|p| p.id),
            root_folder_id: self.root_folders().await.map_err(s)?.first().map(|r| r.id),
        })
    }

    async fn add_artist(&self, req: AddArtistRequest) -> Result<String, AddError> {
        MusicService::add_artist(self, req)
            .await
            .map(|a| a.name)
            .map_err(add_error)
    }

    async fn add_album(&self, req: AddAlbumRequest) -> Result<String, AddError> {
        MusicService::add_album(self, req)
            .await
            .map(|a| a.title)
            .map_err(add_error)
    }
}

/// What album entries add.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlbumHandling {
    /// The album only (its artist is created unmonitored when missing).
    #[default]
    Album,
    /// The album's artist, with the list's artist monitor option.
    Artist,
}

/// Per-list add options of a music list (fields added by the providers crate).
#[derive(Debug, Clone, PartialEq)]
pub struct MusicListOptions {
    pub enabled_auto_add: bool,
    pub quality_profile_id: Option<i64>,
    pub metadata_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub artist_monitor: ArtistMonitor,
    pub monitor_new_items: NewAlbumMonitor,
    pub album_handling: AlbumHandling,
    pub search_on_add: bool,
    pub tags: Vec<i64>,
}

fn enum_setting<T: serde::de::DeserializeOwned + Default>(s: &Map<String, Value>, key: &str) -> T {
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

impl MusicListOptions {
    pub fn from_row(row: &ProviderRow) -> Self {
        let s = &row.settings;
        Self {
            enabled_auto_add: row.setting_bool("enabled_auto_add", true),
            quality_profile_id: id_setting(s, "quality_profile_id"),
            metadata_profile_id: id_setting(s, "metadata_profile_id"),
            root_folder_id: id_setting(s, "root_folder_id"),
            artist_monitor: enum_setting(s, "artist_monitor"),
            monitor_new_items: enum_setting(s, "monitor_new_items"),
            album_handling: enum_setting(s, "album_handling"),
            search_on_add: row.setting_bool("search_on_add", true),
            tags: row.tags.clone(),
        }
    }
}

// ---------------------------------------------------------------- exclusions repo

fn kind_str(k: MusicExclusionKind) -> &'static str {
    match k {
        MusicExclusionKind::Artist => "artist",
        MusicExclusionKind::Album => "album",
    }
}

type ExclusionRow = (i64, String, String, String, i64);

fn exclusion_dto(r: ExclusionRow) -> MusicListExclusionDto {
    MusicListExclusionDto {
        id: r.0,
        kind: if r.1 == "album" {
            MusicExclusionKind::Album
        } else {
            MusicExclusionKind::Artist
        },
        musicbrainz_id: r.2,
        name: r.3,
        created_at: from_ms(r.4),
    }
}

pub async fn list_exclusions(db: &Db) -> sqlx::Result<Vec<MusicListExclusionDto>> {
    let rows: Vec<ExclusionRow> = sqlx::query_as(
        "SELECT id, kind, musicbrainz_id, name, created_at FROM music_list_exclusions
         ORDER BY created_at DESC, id DESC",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(exclusion_dto).collect())
}

/// Inserts an exclusion; `None` when it already exists.
pub async fn add_exclusion(
    db: &Db,
    req: &CreateMusicExclusionRequest,
) -> sqlx::Result<Option<MusicListExclusionDto>> {
    let row: Option<ExclusionRow> = sqlx::query_as(
        "INSERT INTO music_list_exclusions (kind, musicbrainz_id, name, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (kind, musicbrainz_id) DO NOTHING
         RETURNING id, kind, musicbrainz_id, name, created_at",
    )
    .bind(kind_str(req.kind))
    .bind(req.musicbrainz_id.trim().to_ascii_lowercase())
    .bind(req.name.trim())
    .bind(now_ms())
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(exclusion_dto))
}

/// Deletes the given exclusions (empty = all).
pub async fn delete_exclusions(db: &Db, ids: &[i64]) -> sqlx::Result<u64> {
    if ids.is_empty() {
        return Ok(sqlx::query("DELETE FROM music_list_exclusions")
            .execute(db.pool())
            .await?
            .rows_affected());
    }
    let mut n = 0;
    for id in ids {
        n += sqlx::query("DELETE FROM music_list_exclusions WHERE id = ?")
            .bind(id)
            .execute(db.pool())
            .await?
            .rows_affected();
    }
    Ok(n)
}

/// `(kind, lower-case mbid)` of every exclusion.
async fn excluded_set(db: &Db) -> sqlx::Result<HashSet<(MusicItemKind, String)>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, musicbrainz_id FROM music_list_exclusions")
            .fetch_all(db.pool())
            .await?;
    Ok(rows
        .into_iter()
        .map(|(k, m)| {
            let kind = if k == "album" {
                MusicItemKind::Album
            } else {
                MusicItemKind::Artist
            };
            (kind, m.to_ascii_lowercase())
        })
        .collect())
}

// ---------------------------------------------------------------- matching

/// Lower-case alphanumerics, a leading "the" dropped ("The Beatles" == "beatles").
pub fn normalize(s: &str) -> String {
    let words: Vec<String> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let skip = usize::from(words.len() > 1 && words[0] == "the");
    words[skip..].concat()
}

async fn resolve_artist(music: &dyn MusicLibrary, item: &MusicListItem) -> Option<String> {
    if let Some(m) = &item.artist_mbid {
        return Some(m.clone());
    }
    let name = item.artist_name.trim();
    if name.is_empty() {
        return None;
    }
    let want = normalize(name);
    let hits = music.search_artists(name).await.ok()?;
    hits.into_iter()
        .find(|h| normalize(&h.name) == want)
        .map(|h| h.musicbrainz_id.to_ascii_lowercase())
}

/// (release-group mbid, artist mbid).
async fn resolve_album(
    music: &dyn MusicLibrary,
    item: &MusicListItem,
    artist_mbid: Option<&str>,
) -> Option<(String, Option<String>)> {
    if let Some(rg) = &item.album_mbid {
        return Some((rg.clone(), artist_mbid.map(String::from)));
    }
    let title = item.album_title.as_deref()?.trim();
    if title.is_empty() {
        return None;
    }
    let want = normalize(title);
    let want_artist = normalize(&item.artist_name);
    let hits = music.search_albums(title).await.ok()?;
    let matching: Vec<&AlbumLookupDto> = hits
        .iter()
        .filter(|h| normalize(&h.title) == want)
        .filter(|h| match artist_mbid {
            Some(a) => h.artist_musicbrainz_id.eq_ignore_ascii_case(a),
            None => normalize(&h.artist_name) == want_artist,
        })
        .collect();
    let best = matching
        .iter()
        .find(|h| matches!((item.year, h.year), (Some(a), Some(b)) if (a - b).abs() <= 1))
        .or_else(|| matching.first())?;
    Some((
        best.musicbrainz_release_group_id.to_ascii_lowercase(),
        Some(best.artist_musicbrainz_id.to_ascii_lowercase()).filter(|s| !s.is_empty()),
    ))
}

// ---------------------------------------------------------------- sync

/// Adds a music list's items; counts go to `o` (a missing target sets `o.error` once).
pub async fn add_music_items(
    db: &Db,
    music: &dyn MusicLibrary,
    list_name: &str,
    opts: &MusicListOptions,
    items: Vec<MusicListItem>,
    o: &mut ListSyncOutcome,
) -> sqlx::Result<()> {
    let excluded = excluded_set(db).await?;
    let mut seen_artists: HashSet<String> = HashSet::new();
    let mut seen_albums: HashSet<String> = HashSet::new();
    let mut defaults: Option<MusicDefaults> = None;
    for item in items {
        let artist = resolve_artist(music, &item).await;
        let add_artist = item.kind == MusicItemKind::Artist || opts.album_handling == AlbumHandling::Artist;
        if add_artist {
            // Album entries resolve their artist through the album when the name lookup fails.
            let artist = match (artist, item.kind) {
                (Some(a), _) => Some(a),
                (None, MusicItemKind::Album) => resolve_album(music, &item, None).await.and_then(|(_, a)| a),
                (None, MusicItemKind::Artist) => None,
            };
            let Some(mbid) = artist else {
                tracing::debug!(list = %list_name, item = %item.label(), "music list artist not found on MusicBrainz");
                o.unresolved += 1;
                continue;
            };
            if excluded.contains(&(MusicItemKind::Artist, mbid.clone())) {
                o.excluded += 1;
                continue;
            }
            if !seen_artists.insert(mbid.clone()) || music.has_artist(&mbid).await.unwrap_or(false) {
                o.existing += 1;
                continue;
            }
            if defaults.is_none() {
                defaults = Some(music.defaults().await.unwrap_or_default());
            }
            let d = defaults.unwrap_or_default();
            let (Some(qp), Some(mp), Some(root)) = (
                opts.quality_profile_id.or(d.quality_profile_id),
                opts.metadata_profile_id.or(d.metadata_profile_id),
                opts.root_folder_id.or(d.root_folder_id),
            ) else {
                if o.error.is_none() {
                    let msg = "cannot add artists: no music quality profile, metadata profile or music root folder configured".to_string();
                    tracing::warn!(list = %list_name, "{msg}");
                    o.error = Some(msg);
                }
                continue;
            };
            let req = AddArtistRequest {
                musicbrainz_id: mbid.clone(),
                quality_profile_id: qp,
                metadata_profile_id: mp,
                root_folder_id: root,
                monitored: opts.artist_monitor != ArtistMonitor::None,
                monitor: opts.artist_monitor,
                monitor_new_items: opts.monitor_new_items,
                selected_release_group_ids: Vec::new(),
                tags: opts.tags.clone(),
                search_on_add: opts.search_on_add,
            };
            match music.add_artist(req).await {
                Ok(name) => {
                    o.added += 1;
                    tracing::info!(list = %list_name, artist = %name, %mbid, "artist added from import list");
                }
                Err(AddError::Exists) => o.existing += 1,
                Err(AddError::Failed(e)) => {
                    tracing::warn!(list = %list_name, item = %item.label(), %mbid, error = %e, "import list artist add failed");
                }
            }
            continue;
        }

        // Album entry, album handling.
        if let Some(a) = &artist
            && excluded.contains(&(MusicItemKind::Artist, a.clone()))
        {
            o.excluded += 1;
            continue;
        }
        let Some((rg, album_artist)) = resolve_album(music, &item, artist.as_deref()).await else {
            tracing::debug!(list = %list_name, item = %item.label(), "music list album not found on MusicBrainz");
            o.unresolved += 1;
            continue;
        };
        if excluded.contains(&(MusicItemKind::Album, rg.clone()))
            || album_artist.is_some_and(|a| excluded.contains(&(MusicItemKind::Artist, a)))
        {
            o.excluded += 1;
            continue;
        }
        if !seen_albums.insert(rg.clone()) || music.has_album(&rg).await.unwrap_or(false) {
            o.existing += 1;
            continue;
        }
        let req = AddAlbumRequest {
            musicbrainz_release_group_id: rg.clone(),
            quality_profile_id: opts.quality_profile_id,
            metadata_profile_id: opts.metadata_profile_id,
            root_folder_id: opts.root_folder_id,
            tags: opts.tags.clone(),
            search_on_add: opts.search_on_add,
            added_by_request_id: None,
        };
        match music.add_album(req).await {
            Ok(title) => {
                o.added += 1;
                tracing::info!(list = %list_name, album = %title, %rg, "album added from import list");
            }
            Err(AddError::Exists) => o.existing += 1,
            Err(AddError::Failed(e)) => {
                tracing::warn!(list = %list_name, item = %item.label(), %rg, error = %e, "import list album add failed");
                if e.contains("no music") || e.contains("no metadata profile") {
                    o.error.get_or_insert(format!("cannot add albums: {e}"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use reelix_api_types::music::AlbumType;

    use super::*;

    const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
    const BJORK: &str = "87c5dedd-371d-4a53-9f7f-80522fb7f3cb";
    const BEATLES: &str = "b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d";
    const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
    const POST: &str = "0b2bba1a-ae1d-37a1-9d4b-4b0d5c7f5d0e";
    const ABBEY: &str = "9162580e-5df4-32de-80cc-f45a8d8a9b1d";

    fn artist_hit(mbid: &str, name: &str) -> ArtistLookupDto {
        ArtistLookupDto {
            musicbrainz_id: mbid.into(),
            name: name.into(),
            sort_name: name.into(),
            disambiguation: None,
            artist_type: None,
            country: None,
            image: None,
            artist_id: None,
        }
    }

    fn album_hit(rg: &str, title: &str, artist: &str, artist_name: &str, year: i32) -> AlbumLookupDto {
        AlbumLookupDto {
            musicbrainz_release_group_id: rg.into(),
            title: title.into(),
            artist_name: artist_name.into(),
            artist_musicbrainz_id: artist.into(),
            album_type: AlbumType::Album,
            secondary_types: vec![],
            year: Some(year),
            cover: None,
            album_id: None,
        }
    }

    /// In-memory music library.
    #[derive(Default)]
    struct FakeMusic {
        artists: Mutex<Vec<String>>,
        albums: Mutex<Vec<String>>,
        added_artists: Mutex<Vec<AddArtistRequest>>,
        added_albums: Mutex<Vec<AddAlbumRequest>>,
        no_defaults: bool,
    }

    #[async_trait::async_trait]
    impl MusicLibrary for FakeMusic {
        async fn has_artist(&self, mbid: &str) -> Result<bool, String> {
            Ok(self.artists.lock().unwrap().iter().any(|a| a == mbid))
        }
        async fn has_album(&self, rg: &str) -> Result<bool, String> {
            Ok(self.albums.lock().unwrap().iter().any(|a| a == rg))
        }
        async fn search_artists(&self, name: &str) -> Result<Vec<ArtistLookupDto>, String> {
            Ok([
                artist_hit(RADIOHEAD, "Radiohead"),
                artist_hit(BJORK, "Björk"),
                artist_hit(BEATLES, "The Beatles"),
            ]
            .into_iter()
            .filter(|h| {
                h.name.to_lowercase().contains(&name.to_lowercase()) || normalize(&h.name) == normalize(name)
            })
            .collect())
        }
        async fn search_albums(&self, _title: &str) -> Result<Vec<AlbumLookupDto>, String> {
            Ok(vec![
                album_hit(
                    "00000000-0000-0000-0000-000000000001",
                    "OK Computer",
                    BJORK,
                    "Tribute",
                    2020,
                ),
                album_hit(OK_COMPUTER, "OK Computer", RADIOHEAD, "Radiohead", 1997),
                album_hit(POST, "Post", BJORK, "Björk", 1995),
                album_hit(ABBEY, "Abbey Road", BEATLES, "The Beatles", 1969),
            ])
        }
        async fn defaults(&self) -> Result<MusicDefaults, String> {
            Ok(if self.no_defaults {
                MusicDefaults::default()
            } else {
                MusicDefaults {
                    quality_profile_id: Some(7),
                    metadata_profile_id: Some(8),
                    root_folder_id: Some(9),
                }
            })
        }
        async fn add_artist(&self, req: AddArtistRequest) -> Result<String, AddError> {
            self.artists.lock().unwrap().push(req.musicbrainz_id.clone());
            self.added_artists.lock().unwrap().push(req);
            Ok("artist".into())
        }
        async fn add_album(&self, req: AddAlbumRequest) -> Result<String, AddError> {
            self.albums
                .lock()
                .unwrap()
                .push(req.musicbrainz_release_group_id.clone());
            self.added_albums.lock().unwrap().push(req);
            Ok("album".into())
        }
    }

    fn opts() -> MusicListOptions {
        MusicListOptions {
            enabled_auto_add: true,
            quality_profile_id: None,
            metadata_profile_id: Some(3),
            root_folder_id: None,
            artist_monitor: ArtistMonitor::Future,
            monitor_new_items: NewAlbumMonitor::New,
            album_handling: AlbumHandling::Album,
            search_on_add: false,
            tags: vec![4],
        }
    }

    async fn exclude(db: &Db, kind: MusicExclusionKind, mbid: &str) {
        let req = CreateMusicExclusionRequest {
            kind,
            musicbrainz_id: mbid.to_uppercase(),
            name: "x".into(),
        };
        assert!(add_exclusion(db, &req).await.unwrap().is_some());
        assert!(add_exclusion(db, &req).await.unwrap().is_none(), "duplicate");
    }

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize("The Beatles"), normalize("beatles"));
        assert_eq!(normalize("Sigur Rós"), normalize("sigur rós!"));
        assert_eq!(normalize("The The"), "the");
        assert_ne!(normalize("Radiohead"), normalize("Radio"));
    }

    #[tokio::test]
    async fn adds_artists_and_honours_exclusions_and_library() {
        let db = Db::in_memory().await.unwrap();
        exclude(&db, MusicExclusionKind::Artist, BJORK).await;
        let music = FakeMusic::default();
        music.artists.lock().unwrap().push(BEATLES.into());
        let items = vec![
            MusicListItem::artist("Radiohead", None),
            MusicListItem::artist("Radiohead", Some(RADIOHEAD.into())), // same artist again
            MusicListItem::artist("Björk", Some(BJORK.into())),         // excluded
            MusicListItem::artist("Beatles", None),                     // in library ("The" ignored)
            MusicListItem::artist("Nobody Knows Me", None),             // unresolved
        ];
        let mut o = ListSyncOutcome::default();
        add_music_items(&db, &music, "lb", &opts(), items, &mut o)
            .await
            .unwrap();
        assert_eq!((o.added, o.existing, o.excluded, o.unresolved), (1, 2, 1, 1));
        assert_eq!(o.error, None);
        let added = music.added_artists.lock().unwrap().clone();
        assert_eq!(added.len(), 1);
        let a = &added[0];
        assert_eq!(a.musicbrainz_id, RADIOHEAD);
        assert_eq!(
            (a.quality_profile_id, a.metadata_profile_id, a.root_folder_id),
            (7, 3, 9)
        );
        assert_eq!(
            (a.monitor, a.monitor_new_items, a.monitored),
            (ArtistMonitor::Future, NewAlbumMonitor::New, true)
        );
        assert_eq!(a.tags, [4]);

        // Second sync: nothing new.
        let mut o = ListSyncOutcome::default();
        let again = vec![MusicListItem::artist("Radiohead", Some(RADIOHEAD.into()))];
        add_music_items(&db, &music, "lb", &opts(), again, &mut o)
            .await
            .unwrap();
        assert_eq!((o.added, o.existing), (0, 1));
    }

    #[tokio::test]
    async fn adds_albums_by_mbid_or_title_and_skips_excluded() {
        let db = Db::in_memory().await.unwrap();
        exclude(&db, MusicExclusionKind::Album, ABBEY).await;
        let music = FakeMusic::default();
        music.albums.lock().unwrap().push(POST.into());
        let items = vec![
            MusicListItem::album("OK Computer", "Radiohead", None), // title match, Radiohead's (not the tribute)
            MusicListItem::album("Post", "Björk", Some(POST.into())), // already in library
            MusicListItem::album("Abbey Road", "The Beatles", None), // excluded album
            MusicListItem::album("Unknown", "Radiohead", None),     // unresolved
        ];
        let mut o = ListSyncOutcome::default();
        add_music_items(&db, &music, "lfm", &opts(), items, &mut o)
            .await
            .unwrap();
        assert_eq!((o.added, o.existing, o.excluded, o.unresolved), (1, 1, 1, 1));
        let added = music.added_albums.lock().unwrap().clone();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].musicbrainz_release_group_id, OK_COMPUTER);
        assert_eq!(added[0].metadata_profile_id, Some(3));
        assert_eq!(added[0].quality_profile_id, None);

        let excl = list_exclusions(&db).await.unwrap();
        assert_eq!(excl.len(), 1);
        assert_eq!(excl[0].musicbrainz_id, ABBEY);
        assert_eq!(delete_exclusions(&db, &[]).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn album_entries_can_add_their_artist_and_missing_targets_error() {
        let db = Db::in_memory().await.unwrap();
        let music = FakeMusic {
            no_defaults: true,
            ..Default::default()
        };
        let mut o = MusicListOptions {
            album_handling: AlbumHandling::Artist,
            ..opts()
        };
        let items = || vec![MusicListItem::album("Post", "Björk", None)];
        let mut out = ListSyncOutcome::default();
        add_music_items(&db, &music, "sp", &o, items(), &mut out)
            .await
            .unwrap();
        assert_eq!(out.added, 0);
        assert!(out.error.as_deref().unwrap().contains("no music quality profile"));

        o.quality_profile_id = Some(1);
        o.root_folder_id = Some(2);
        let mut out = ListSyncOutcome::default();
        add_music_items(&db, &music, "sp", &o, items(), &mut out)
            .await
            .unwrap();
        assert_eq!((out.added, out.error.clone()), (1, None));
        assert_eq!(music.added_artists.lock().unwrap()[0].musicbrainz_id, BJORK);
        assert!(music.added_albums.lock().unwrap().is_empty());
    }
}
