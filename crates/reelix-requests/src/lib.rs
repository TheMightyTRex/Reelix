//! Requests, approvals, quotas, discovery, issues, user management and external login (M6).
//!
//! Everything is exposed through one [`RequestService`]. It talks to the library, media
//! servers, metadata and notifiers only through the traits in [`gateway`], so this crate
//! doesn't depend on `reelix-library` or `reelix-providers`.

pub mod discovery;
pub mod external;
pub mod fakes;
pub mod gateway;
pub mod issues;
pub mod jobs;
pub mod oidc;
pub mod repo;
pub mod requests;
pub mod settings;
pub mod users;

use std::sync::{Arc, OnceLock};

use reelix_core::bus::EventBus;
use reelix_core::{DomainEvent, User};
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_jobs::JobQueue;

pub use gateway::{
    AddMovieRequest, AddSeriesRequest, Availability, DiscoverySource, LibraryGateway, MediaServerDirectory,
    MediaServerEntry, NotificationSink, ResolvingSource,
};
pub use jobs::{REQUEST_STATUS_SYNC, RequestStatusSyncJob, WATCHLIST_SYNC, WatchlistSyncJob, tasks};
pub use reelix_mediaserver::MediaServerKind;
/// Re-exported for server adapters.
pub use {reelix_mediaserver as mediaserver, reelix_metadata as metadata, reelix_notify as notify};

/// Errors returned by the service; the API maps them to HTTP statuses.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    NotFound(String),
    /// 403 with a message (missing permission, quota exceeded, …).
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    /// External login rejected (401).
    #[error("{0}")]
    Unauthorized(String),
    /// A dependency (metadata, media server, identity provider, library) failed (502).
    #[error("{0}")]
    Upstream(String),
    #[error(transparent)]
    Db(#[from] reelix_db::DbError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e.into())
    }
}

impl From<reelix_metadata::MetaError> for Error {
    fn from(e: reelix_metadata::MetaError) -> Self {
        match e {
            reelix_metadata::MetaError::NotFound => Self::NotFound("title not found".into()),
            reelix_metadata::MetaError::NotConfigured(p) => Self::Upstream(format!(
                "{p} is not configured: add an API key in Settings → Metadata"
            )),
            other => Self::Upstream(format!("metadata provider error: {other}")),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Dependencies of [`RequestService`].
pub struct RequestDeps {
    pub db: Db,
    pub events: EventBus,
    pub secrets: SecretBox,
    pub library: Arc<dyn LibraryGateway>,
    pub media_servers: Arc<dyn MediaServerDirectory>,
    pub metadata: Arc<dyn DiscoverySource>,
    pub notifications: Arc<dyn NotificationSink>,
    /// HTTP client for OIDC (redirects are disabled internally).
    pub http: Option<reqwest::Client>,
}

/// The requests domain service (registered on the API state as an extension).
pub struct RequestService {
    pub(crate) db: Db,
    pub(crate) events: EventBus,
    pub(crate) secrets: SecretBox,
    pub(crate) library: Arc<dyn LibraryGateway>,
    pub(crate) media_servers: Arc<dyn MediaServerDirectory>,
    pub(crate) metadata: Arc<dyn DiscoverySource>,
    pub(crate) notifications: Arc<dyn NotificationSink>,
    pub(crate) oidc: oidc::OidcState,
    queue: OnceLock<JobQueue>,
}

impl RequestService {
    pub fn new(deps: RequestDeps) -> Self {
        Self {
            oidc: oidc::OidcState::new(deps.http),
            db: deps.db,
            events: deps.events,
            secrets: deps.secrets,
            library: deps.library,
            media_servers: deps.media_servers,
            metadata: deps.metadata,
            notifications: deps.notifications,
            queue: OnceLock::new(),
        }
    }

    /// Gives the service the job queue (server `attach` phase).
    pub fn set_queue(&self, queue: JobQueue) {
        let _ = self.queue.set(queue);
    }

    pub fn queue(&self) -> Option<&JobQueue> {
        self.queue.get()
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub(crate) fn changed(&self, entity: &str, action: &str, id: i64, user_id: Option<i64>) {
        self.events.publish(DomainEvent::EntityChanged {
            entity: entity.into(),
            action: action.into(),
            id,
            user_id,
        });
    }
}

/// The acting user (from the API's `CurrentUser`).
pub type Actor = User;
