//! Trait boundaries to the rest of the system. The server adapts the library, provider and
//! notification services to these traits; tests use the in-memory fakes in [`crate::fakes`].

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use reelix_api_types::requests::{AvailabilityStatus, RootFolderOptionDto};
use reelix_core::MediaKind;
use reelix_mediaserver::plextv::PlexTv;
use reelix_mediaserver::{MediaServer, MediaServerKind};
use reelix_metadata::{
    Genre, MetaError, MovieMetadata, MultiHit, Page, SearchHit, SeriesMetadata, Tmdb, TmdbId,
};
use reelix_notify::NotificationEvent;

pub use reelix_metadata::DiscoverFeed;

/// Library state of one title.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Availability {
    pub status: AvailabilityStatus,
    /// Library id (movie id / series id).
    pub library_id: Option<i64>,
    /// Series: per-season status for seasons the library knows about.
    pub seasons: BTreeMap<u32, AvailabilityStatus>,
}

/// Adds an approved movie request to the library.
#[derive(Debug, Clone, PartialEq)]
pub struct AddMovieRequest {
    pub tmdb_id: TmdbId,
    pub title: String,
    pub year: Option<i32>,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
    /// `announced`, `in_cinemas` or `released`.
    pub minimum_availability: String,
    pub is_4k: bool,
    pub request_id: i64,
}

/// Adds (or extends) a series for an approved request.
#[derive(Debug, Clone, PartialEq)]
pub struct AddSeriesRequest {
    pub tmdb_id: TmdbId,
    pub tvdb_id: Option<u32>,
    pub title: String,
    pub year: Option<i32>,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub tags: Vec<i64>,
    /// Seasons to monitor. If the series already exists these are monitored additionally.
    pub seasons: Vec<u32>,
    pub season_folder: bool,
    /// `standard`, `daily` or `anime`.
    pub series_type: String,
    pub is_4k: bool,
    pub request_id: i64,
}

/// The library as seen by requests (implemented by the server over `reelix-library`).
#[async_trait]
pub trait LibraryGateway: Send + Sync {
    /// Availability of the given TMDB ids; ids missing from the map are not in the library.
    async fn availability(
        &self,
        kind: MediaKind,
        tmdb_ids: &[TmdbId],
    ) -> anyhow::Result<HashMap<TmdbId, Availability>>;

    /// Adds the movie (or returns the existing one's id). Returns the library movie id.
    async fn add_movie(&self, req: AddMovieRequest) -> anyhow::Result<i64>;

    /// Adds the series or monitors the extra seasons of an existing one. Returns the series id.
    async fn add_series(&self, req: AddSeriesRequest) -> anyhow::Result<i64>;

    /// Starts a search for monitored, missing items of these library ids.
    async fn request_search(&self, kind: MediaKind, ids: &[i64]) -> anyhow::Result<()>;

    /// Root folders for the advanced request options.
    async fn root_folders(&self, kind: MediaKind) -> anyhow::Result<Vec<RootFolderOptionDto>>;
}

/// A configured media server.
#[derive(Clone)]
pub struct MediaServerEntry {
    /// Provider id.
    pub id: i64,
    pub name: String,
    pub kind: MediaServerKind,
    pub server: Arc<dyn MediaServer>,
}

/// Configured media servers (implemented by the server over `reelix-providers`).
#[async_trait]
pub trait MediaServerDirectory: Send + Sync {
    /// Enabled media servers.
    async fn servers(&self) -> Vec<MediaServerEntry>;
    /// plex.tv client (PIN login, account lookups, watchlists).
    fn plex_tv(&self) -> PlexTv;
}

/// Metadata used for discovery and media details.
#[async_trait]
pub trait DiscoverySource: Send + Sync {
    async fn discover(&self, feed: &DiscoverFeed, page: u32) -> Result<Page<SearchHit>, MetaError>;
    async fn search(&self, query: &str, page: u32) -> Result<Page<MultiHit>, MetaError>;
    /// Movie details including recommendations/similar.
    async fn movie(&self, id: TmdbId) -> Result<MovieMetadata, MetaError>;
    /// Series details including seasons, recommendations/similar.
    async fn series(&self, id: TmdbId) -> Result<SeriesMetadata, MetaError>;
    /// Recommendations (`similar = false`) or similar titles.
    async fn related(
        &self,
        kind: MediaKind,
        id: TmdbId,
        similar: bool,
        page: u32,
    ) -> Result<Page<SearchHit>, MetaError>;
    async fn genres(&self, kind: MediaKind) -> Result<Vec<Genre>, MetaError>;
}

#[async_trait]
impl DiscoverySource for Tmdb {
    async fn discover(&self, feed: &DiscoverFeed, page: u32) -> Result<Page<SearchHit>, MetaError> {
        self.discover_feed(feed, page).await
    }
    async fn search(&self, query: &str, page: u32) -> Result<Page<MultiHit>, MetaError> {
        self.search_multi(query, page).await
    }
    async fn movie(&self, id: TmdbId) -> Result<MovieMetadata, MetaError> {
        self.movie_details(id).await
    }
    async fn series(&self, id: TmdbId) -> Result<SeriesMetadata, MetaError> {
        self.tv_details(id).await
    }
    async fn related(
        &self,
        kind: MediaKind,
        id: TmdbId,
        similar: bool,
        page: u32,
    ) -> Result<Page<SearchHit>, MetaError> {
        if similar {
            Tmdb::similar(self, kind, id, page).await
        } else {
            Tmdb::recommendations(self, kind, id, page).await
        }
    }
    async fn genres(&self, kind: MediaKind) -> Result<Vec<Genre>, MetaError> {
        Tmdb::genres(self, kind).await
    }
}

/// Resolves the current metadata source on every call (settings may change at runtime).
pub type SourceResolver = Arc<dyn Fn() -> Result<Arc<dyn DiscoverySource>, MetaError> + Send + Sync>;

/// A [`DiscoverySource`] that asks a resolver for the real source on each call, e.g. the
/// provider service's current TMDB client. Errors with `NotConfigured` until one exists.
pub struct ResolvingSource(pub SourceResolver);

impl ResolvingSource {
    fn get(&self) -> Result<Arc<dyn DiscoverySource>, MetaError> {
        (self.0)()
    }
}

#[async_trait]
impl DiscoverySource for ResolvingSource {
    async fn discover(&self, feed: &DiscoverFeed, page: u32) -> Result<Page<SearchHit>, MetaError> {
        self.get()?.discover(feed, page).await
    }
    async fn search(&self, query: &str, page: u32) -> Result<Page<MultiHit>, MetaError> {
        self.get()?.search(query, page).await
    }
    async fn movie(&self, id: TmdbId) -> Result<MovieMetadata, MetaError> {
        self.get()?.movie(id).await
    }
    async fn series(&self, id: TmdbId) -> Result<SeriesMetadata, MetaError> {
        self.get()?.series(id).await
    }
    async fn related(
        &self,
        kind: MediaKind,
        id: TmdbId,
        similar: bool,
        page: u32,
    ) -> Result<Page<SearchHit>, MetaError> {
        self.get()?.related(kind, id, similar, page).await
    }
    async fn genres(&self, kind: MediaKind) -> Result<Vec<Genre>, MetaError> {
        self.get()?.genres(kind).await
    }
}

/// Delivers request/issue notifications (implemented by the server over the notifiers).
#[async_trait]
pub trait NotificationSink: Send + Sync {
    /// `recipients` are users who opted in to this event in their preferences (requester for
    /// approved/declined/available; managers for pending/reported).
    async fn notify(&self, event: NotificationEvent, recipients: Vec<i64>);
}

/// Placeholder implementations used until the server provides real adapters.
pub mod unconnected {
    use super::*;

    /// Library not wired: nothing is in the library and approvals fail.
    pub struct NoLibrary;

    #[async_trait]
    impl LibraryGateway for NoLibrary {
        async fn availability(
            &self,
            _: MediaKind,
            _: &[TmdbId],
        ) -> anyhow::Result<HashMap<TmdbId, Availability>> {
            Ok(HashMap::new())
        }
        async fn add_movie(&self, _: AddMovieRequest) -> anyhow::Result<i64> {
            anyhow::bail!("the library is not connected")
        }
        async fn add_series(&self, _: AddSeriesRequest) -> anyhow::Result<i64> {
            anyhow::bail!("the library is not connected")
        }
        async fn request_search(&self, _: MediaKind, _: &[i64]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn root_folders(&self, _: MediaKind) -> anyhow::Result<Vec<RootFolderOptionDto>> {
            Ok(Vec::new())
        }
    }

    /// No media servers configured.
    pub struct NoMediaServers(pub PlexTv);

    #[async_trait]
    impl MediaServerDirectory for NoMediaServers {
        async fn servers(&self) -> Vec<MediaServerEntry> {
            Vec::new()
        }
        fn plex_tv(&self) -> PlexTv {
            self.0.clone()
        }
    }

    /// Metadata not configured.
    pub fn no_metadata() -> ResolvingSource {
        ResolvingSource(Arc::new(|| Err(MetaError::NotConfigured("TMDB"))))
    }

    /// Drops notifications (logged at debug level).
    pub struct NoNotifications;

    #[async_trait]
    impl NotificationSink for NoNotifications {
        async fn notify(&self, event: NotificationEvent, _: Vec<i64>) {
            tracing::debug!(?event, "notification dropped (no sink)");
        }
    }
}

/// Closure-based adapters so the server can wire other services without new types.
pub mod adapters {
    use std::future::Future;
    use std::pin::Pin;

    use super::*;

    /// Media servers from a synchronous accessor (e.g. the provider service's cache).
    pub struct FnDirectory {
        pub servers: Arc<dyn Fn() -> Vec<MediaServerEntry> + Send + Sync>,
        pub plex_tv: PlexTv,
    }

    #[async_trait]
    impl MediaServerDirectory for FnDirectory {
        async fn servers(&self) -> Vec<MediaServerEntry> {
            (self.servers)()
        }
        fn plex_tv(&self) -> PlexTv {
            self.plex_tv.clone()
        }
    }

    pub type NotifyFn =
        Arc<dyn Fn(NotificationEvent, Vec<i64>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

    /// Notifications through a closure (e.g. the provider service's fan-out).
    pub struct FnNotifications(pub NotifyFn);

    #[async_trait]
    impl NotificationSink for FnNotifications {
        async fn notify(&self, event: NotificationEvent, recipients: Vec<i64>) {
            (self.0)(event, recipients).await
        }
    }
}
