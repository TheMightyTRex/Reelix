//! Import list adapters: TMDB, Trakt, IMDb, MDBList, Plex watchlist, Jellyfin/Emby favourites,
//! generic JSON URL lists and other Reelix instances (movies/series), plus Last.fm, ListenBrainz
//! and Spotify (music: artists and albums, see [`ListContent::Music`] and
//! [`ImportList::fetch_music`]).
//!
//! Every adapter implements [`ImportList`], publishes a [`ProviderSchema`] via [`schemas`] and is
//! constructed from a validated settings object via [`build`]. Upstream base URLs default to the
//! real services ([`BaseUrls`]) and can be overridden per instance (`base_url` setting) or globally
//! through [`BuildContext`], which is how the tests point adapters at wiremock.

mod common;
pub mod imdb;
pub mod jellyfin;
pub mod lastfm;
pub mod listenbrainz;
pub mod mdblist;
pub mod plex;
pub mod reelix;
pub mod spotify;
pub mod tmdb;
pub mod trakt;
pub mod url_list;

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::ProviderSchema;
use serde::{Deserialize, Serialize};

/// Setting name (hours) that controls how often a list is re-synced; present in every schema.
pub const SYNC_INTERVAL_FIELD: &str = "sync_interval_hours";
/// Default sync interval in hours (README: "Import Lists Sync – 6 h").
pub const DEFAULT_SYNC_INTERVAL_HOURS: u64 = 6;

/// One entry of an import list. At least one external id or a title is always present.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ListItem {
    pub kind: MediaKind,
    pub title: String,
    pub year: Option<i32>,
    pub tmdb_id: Option<u64>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u64>,
}

impl ListItem {
    /// Empty item of the given kind; fill fields with struct update syntax.
    pub fn new(kind: MediaKind, title: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            year: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
        }
    }

    /// True when the item carries at least one external id usable for a metadata lookup.
    pub fn has_id(&self) -> bool {
        self.tmdb_id.is_some() || self.imdb_id.is_some() || self.tvdb_id.is_some()
    }
}

/// What a list yields: movies/series ([`ImportList::fetch`]) or music ([`ImportList::fetch_music`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListContent {
    #[default]
    Video,
    Music,
}

impl ListContent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Music => "music",
        }
    }
}

/// Content of an implementation id (without building it); unknown ids are `Video`.
pub fn content_of(implementation: &str) -> ListContent {
    match implementation {
        lastfm::IMPLEMENTATION | listenbrainz::IMPLEMENTATION | spotify::IMPLEMENTATION => ListContent::Music,
        _ => ListContent::Video,
    }
}

/// Whether a music list entry is an artist or an album (release group).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MusicItemKind {
    Artist,
    Album,
}

/// One entry of a music import list. MusicBrainz ids are set when the source knows them
/// (ListenBrainz, Last.fm artists); otherwise the sync matches by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MusicListItem {
    pub kind: MusicItemKind,
    /// Artist name (the album's primary artist for albums).
    pub artist_name: String,
    pub artist_mbid: Option<String>,
    /// Album title (albums only).
    pub album_title: Option<String>,
    /// MusicBrainz release-group id (albums only).
    pub album_mbid: Option<String>,
    pub year: Option<i32>,
    /// Album barcode when the source reports it (Spotify).
    pub upc: Option<String>,
}

impl MusicListItem {
    pub fn artist(name: impl Into<String>, mbid: Option<String>) -> Self {
        Self {
            kind: MusicItemKind::Artist,
            artist_name: name.into(),
            artist_mbid: mbid,
            album_title: None,
            album_mbid: None,
            year: None,
            upc: None,
        }
    }

    pub fn album(
        title: impl Into<String>,
        artist: impl Into<String>,
        release_group_mbid: Option<String>,
    ) -> Self {
        Self {
            kind: MusicItemKind::Album,
            album_title: Some(title.into()),
            album_mbid: release_group_mbid,
            ..Self::artist(artist, None)
        }
    }

    /// Human-readable label for logs (`Artist` or `Artist – Album`).
    pub fn label(&self) -> String {
        match &self.album_title {
            Some(t) => format!("{} – {t}", self.artist_name),
            None => self.artist_name.clone(),
        }
    }
}

/// Removes duplicate music entries (same kind and MBID, else same lower-cased names).
pub fn dedupe_music(items: Vec<MusicListItem>) -> Vec<MusicListItem> {
    let mut seen: HashSet<(MusicItemKind, String)> = HashSet::new();
    items
        .into_iter()
        .filter(|i| {
            let key = match i.kind {
                MusicItemKind::Artist => match &i.artist_mbid {
                    Some(m) => format!("mbid:{m}"),
                    None => format!("name:{}", i.artist_name.to_lowercase()),
                },
                MusicItemKind::Album => match &i.album_mbid {
                    Some(m) => format!("mbid:{m}"),
                    None => format!(
                        "name:{}:{}",
                        i.artist_name.to_lowercase(),
                        i.album_title.as_deref().unwrap_or_default().to_lowercase()
                    ),
                },
            };
            seen.insert((i.kind, key))
        })
        .collect()
}

/// Errors from list adapters.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    #[error("unknown import list implementation: {0}")]
    UnknownImplementation(String),
    #[error("authentication failed (check the API key / token)")]
    Unauthorized,
    /// OAuth device authorization ended without a token (denied, expired, …).
    #[error("authorization failed: {0}")]
    AuthorizationFailed(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("rate limited by upstream")]
    RateLimited { retry_after: Option<Duration> },
    #[error("unexpected HTTP status {status}: {body}")]
    Status { status: u16, body: String },
    /// Transport error; the URL is stripped so API keys in query strings never leak into logs.
    #[error("http error: {0}")]
    Http(reqwest::Error),
    #[error("failed to parse response: {0}")]
    Parse(String),
}

impl From<reqwest::Error> for ListError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.without_url())
    }
}

/// An import list source.
#[async_trait]
pub trait ImportList: Send + Sync {
    /// Implementation id, matching [`ProviderSchema::implementation`].
    fn implementation(&self) -> &'static str;

    /// What this list yields; music lists implement [`fetch_music`](Self::fetch_music).
    fn content(&self) -> ListContent {
        ListContent::Video
    }

    /// Fetches the full list (all pages up to the adapter's limits), de-duplicated.
    /// Music lists return an empty list here.
    async fn fetch(&self) -> Result<Vec<ListItem>, ListError>;

    /// Fetches a music list's artists / albums, de-duplicated (empty for video lists).
    async fn fetch_music(&self) -> Result<Vec<MusicListItem>, ListError> {
        Ok(Vec::new())
    }

    /// Verifies credentials and that the list exists.
    async fn test(&self) -> Result<(), ListError> {
        self.fetch().await.map(|_| ())
    }
}

/// Default upstream endpoints; override fields to redirect an adapter (tests, proxies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseUrls {
    /// TMDB v3 API root, including the `/3` version prefix.
    pub tmdb: String,
    pub trakt: String,
    pub imdb: String,
    pub mdblist: String,
    /// Plex discover service used for the account watchlist.
    pub plex_discover: String,
    /// Last.fm web service root (`/2.0/` is appended).
    pub lastfm: String,
    pub listenbrainz: String,
    /// Spotify Web API root (`/v1/…` is appended).
    pub spotify_api: String,
    /// Spotify accounts service (`/api/token`).
    pub spotify_accounts: String,
}

impl Default for BaseUrls {
    fn default() -> Self {
        Self {
            tmdb: "https://api.themoviedb.org/3".into(),
            trakt: "https://api.trakt.tv".into(),
            imdb: "https://www.imdb.com".into(),
            mdblist: "https://api.mdblist.com".into(),
            plex_discover: "https://discover.provider.plex.tv".into(),
            lastfm: "https://ws.audioscrobbler.com".into(),
            listenbrainz: "https://api.listenbrainz.org".into(),
            spotify_api: "https://api.spotify.com".into(),
            spotify_accounts: "https://accounts.spotify.com".into(),
        }
    }
}

/// Shared dependencies for building adapters.
#[derive(Debug, Clone)]
pub struct BuildContext {
    pub http: reqwest::Client,
    pub base_urls: BaseUrls,
}

impl BuildContext {
    /// Context with the real upstream endpoints.
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            base_urls: BaseUrls::default(),
        }
    }

    /// Points every hosted service at `base` (used by tests to target a single mock server).
    pub fn with_all_base_urls(mut self, base: &str) -> Self {
        let b = base.trim_end_matches('/').to_string();
        self.base_urls = BaseUrls {
            tmdb: b.clone(),
            trakt: b.clone(),
            imdb: b.clone(),
            mdblist: b.clone(),
            plex_discover: b.clone(),
            lastfm: b.clone(),
            listenbrainz: b.clone(),
            spotify_api: b.clone(),
            spotify_accounts: b,
        };
        self
    }
}

/// One schema per implementation.
pub fn schemas() -> Vec<ProviderSchema> {
    vec![
        tmdb::schema(),
        trakt::schema(),
        imdb::schema(),
        mdblist::schema(),
        plex::schema(),
        jellyfin::schema(jellyfin::Flavor::Jellyfin),
        jellyfin::schema(jellyfin::Flavor::Emby),
        url_list::schema(),
        reelix::schema(),
        lastfm::schema(),
        listenbrainz::schema(),
        spotify::schema(),
    ]
}

/// Builds an adapter from its implementation id and (unvalidated) settings.
pub fn build(
    implementation: &str,
    settings: &serde_json::Value,
    ctx: &BuildContext,
) -> Result<Box<dyn ImportList>, ListError> {
    Ok(match implementation {
        tmdb::IMPLEMENTATION => Box::new(tmdb::TmdbList::from_settings(settings, ctx)?),
        trakt::IMPLEMENTATION => Box::new(trakt::TraktList::from_settings(settings, ctx)?),
        imdb::IMPLEMENTATION => Box::new(imdb::ImdbList::from_settings(settings, ctx)?),
        mdblist::IMPLEMENTATION => Box::new(mdblist::MdbList::from_settings(settings, ctx)?),
        plex::IMPLEMENTATION => Box::new(plex::PlexWatchlist::from_settings(settings, ctx)?),
        "jellyfin" => Box::new(jellyfin::Favourites::from_settings(
            jellyfin::Flavor::Jellyfin,
            settings,
            ctx,
        )?),
        "emby" => Box::new(jellyfin::Favourites::from_settings(
            jellyfin::Flavor::Emby,
            settings,
            ctx,
        )?),
        url_list::IMPLEMENTATION => Box::new(url_list::UrlList::from_settings(settings, ctx)?),
        reelix::IMPLEMENTATION => Box::new(reelix::ReelixList::from_settings(settings, ctx)?),
        lastfm::IMPLEMENTATION => Box::new(lastfm::LastFmList::from_settings(settings, ctx)?),
        listenbrainz::IMPLEMENTATION => {
            Box::new(listenbrainz::ListenBrainzList::from_settings(settings, ctx)?)
        }
        spotify::IMPLEMENTATION => Box::new(spotify::SpotifyList::from_settings(settings, ctx)?),
        other => return Err(ListError::UnknownImplementation(other.into())),
    })
}

/// Sync interval from a stored settings object (defaults to 6 h, never below 1 h).
pub fn sync_interval(settings: &serde_json::Value) -> Duration {
    let hours = settings
        .get(SYNC_INTERVAL_FIELD)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .unwrap_or(DEFAULT_SYNC_INTERVAL_HOURS)
        .max(1);
    Duration::from_secs(hours * 3600)
}

/// Removes duplicates (same kind and any shared external id, or same title+year when id-less),
/// keeping the first occurrence and its order.
pub fn dedupe(items: Vec<ListItem>) -> Vec<ListItem> {
    let mut seen: HashSet<(MediaKind, String)> = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let mut keys = Vec::new();
        if let Some(id) = item.tmdb_id {
            keys.push(format!("tmdb:{id}"));
        }
        if let Some(id) = &item.imdb_id {
            keys.push(format!("imdb:{id}"));
        }
        if let Some(id) = item.tvdb_id {
            keys.push(format!("tvdb:{id}"));
        }
        if keys.is_empty() {
            keys.push(format!("title:{}:{:?}", item.title.to_lowercase(), item.year));
        }
        if keys.iter().any(|k| seen.contains(&(item.kind, k.clone()))) {
            continue;
        }
        seen.extend(keys.into_iter().map(|k| (item.kind, k)));
        out.push(item);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_schema_has_sync_interval_and_builds_unknown_fails() {
        let all = schemas();
        assert_eq!(all.len(), 12);
        let music: Vec<_> = all
            .iter()
            .filter(|s| content_of(&s.implementation) == ListContent::Music)
            .map(|s| s.implementation.as_str())
            .collect();
        assert_eq!(music, ["lastfm", "listenbrainz", "spotify"]);
        for s in &all {
            assert!(
                s.fields.iter().any(|f| f.name == SYNC_INTERVAL_FIELD),
                "{}",
                s.implementation
            );
        }
        let ctx = BuildContext::new(reqwest::Client::new());
        assert!(matches!(
            build("nope", &json!({}), &ctx),
            Err(ListError::UnknownImplementation(_))
        ));
    }

    #[test]
    fn sync_interval_defaults_and_clamps() {
        assert_eq!(sync_interval(&json!({})), Duration::from_secs(6 * 3600));
        assert_eq!(
            sync_interval(&json!({"sync_interval_hours": 0})),
            Duration::from_secs(3600)
        );
        assert_eq!(
            sync_interval(&json!({"sync_interval_hours": "12"})),
            Duration::from_secs(12 * 3600)
        );
    }

    #[test]
    fn dedupe_by_any_id() {
        let a = ListItem {
            tmdb_id: Some(1),
            ..ListItem::new(MediaKind::Movie, "A")
        };
        let b = ListItem {
            tmdb_id: Some(1),
            imdb_id: Some("tt1".into()),
            ..ListItem::new(MediaKind::Movie, "A")
        };
        let c = ListItem {
            imdb_id: Some("tt1".into()),
            ..ListItem::new(MediaKind::Movie, "A2")
        };
        let d = ListItem {
            tmdb_id: Some(1),
            ..ListItem::new(MediaKind::Series, "S")
        };
        let e = ListItem::new(MediaKind::Movie, "X");
        let out = dedupe(vec![a.clone(), b, c.clone(), d.clone(), e.clone(), e.clone()]);
        assert_eq!(out, vec![a, c, d, e]);
    }

    #[test]
    fn dedupe_music_by_mbid_or_name() {
        let a = MusicListItem::artist("Radiohead", Some("m1".into()));
        let b = MusicListItem::artist("radiohead", Some("m1".into()));
        let c = MusicListItem::artist("Radiohead", None);
        let d = MusicListItem::artist("RADIOHEAD", None);
        let e = MusicListItem::album("OK Computer", "Radiohead", None);
        let f = MusicListItem::album("ok computer", "radiohead", None);
        assert_eq!(
            dedupe_music(vec![a.clone(), b, c.clone(), d, e.clone(), f]),
            vec![a, c, e]
        );
    }
}
