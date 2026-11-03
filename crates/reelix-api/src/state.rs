use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_jobs::JobQueue;

use crate::auth::{LoginLimiter, SecurityConfig};

/// Static configuration the API needs from the server.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// `""` or a path like `/reelix` (no trailing slash).
    pub url_base: String,
    pub data_dir: PathBuf,
    /// Database engine name, e.g. `sqlite`.
    pub database: String,
    pub start_time: DateTime<Utc>,
}

/// Shared state handed to every handler. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub db: Db,
    pub events: EventBus,
    pub jobs: JobQueue,
    pub secrets: SecretBox,
    pub config: ApiConfig,
    pub limiter: LoginLimiter,
    pub security: SecurityConfig,
    /// One-time token first-run setup requires from non-local clients (`None`: not required).
    pub setup_token: Option<String>,
    /// Serialises first-run setup so two concurrent requests cannot both create an admin.
    pub setup_lock: tokio::sync::Mutex<()>,
    /// Domain services registered by the server (library, providers, requests, …), keyed by type.
    pub extensions: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl AppState {
    pub fn new(db: Db, events: EventBus, jobs: JobQueue, secrets: SecretBox, config: ApiConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                events,
                jobs,
                secrets,
                config,
                limiter: LoginLimiter::default(),
                security: SecurityConfig::default(),
                setup_token: None,
                setup_lock: tokio::sync::Mutex::new(()),
                extensions: HashMap::new(),
            }),
        }
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn events(&self) -> &EventBus {
        &self.inner.events
    }

    pub fn jobs(&self) -> &JobQueue {
        &self.inner.jobs
    }

    pub fn secrets(&self) -> &SecretBox {
        &self.inner.secrets
    }

    pub fn config(&self) -> &ApiConfig {
        &self.inner.config
    }

    pub fn security(&self) -> &SecurityConfig {
        &self.inner.security
    }

    fn inner_mut(&mut self) -> &mut Inner {
        Arc::get_mut(&mut self.inner).expect("AppState must be configured before it is cloned")
    }

    /// Sets the trusted proxies and cookie policy (before building the router).
    pub fn with_security(mut self, security: SecurityConfig) -> Self {
        self.inner_mut().security = security;
        self
    }

    /// Requires `token` in `POST /setup` from clients other than the server machine itself.
    pub fn with_setup_token(mut self, token: impl Into<String>) -> Self {
        self.inner_mut().setup_token = Some(token.into());
        self
    }

    /// Registers a domain service that handlers fetch with [`AppState::ext`].
    ///
    /// # Panics
    /// If called after the state has been cloned (register everything before building the router).
    pub fn with_extension<T: Send + Sync + 'static>(mut self, service: Arc<T>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("extensions must be registered before AppState is cloned")
            .extensions
            .insert(TypeId::of::<T>(), service);
        self
    }

    /// A registered domain service, or `None` if the server didn't register one.
    pub fn try_ext<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.inner
            .extensions
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|s| s.downcast::<T>().ok())
    }

    /// A registered domain service; a missing one is an internal error (500).
    pub fn ext<T: Send + Sync + 'static>(&self) -> crate::ApiResult<Arc<T>> {
        self.try_ext::<T>().ok_or_else(|| {
            crate::ApiError::internal(anyhow::anyhow!(
                "service {} is not registered",
                std::any::type_name::<T>()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Svc(u32);

    #[tokio::test]
    async fn extensions_roundtrip() {
        let state = AppState::new(
            Db::in_memory().await.unwrap(),
            EventBus::default(),
            reelix_jobs::JobSystem::builder(Db::in_memory().await.unwrap(), EventBus::default())
                .build()
                .queue(),
            SecretBox::from_key([7; 32]),
            ApiConfig {
                url_base: String::new(),
                data_dir: PathBuf::from("."),
                database: "sqlite".into(),
                start_time: Utc::now(),
            },
        )
        .with_extension(Arc::new(Svc(42)));
        assert_eq!(state.ext::<Svc>().unwrap().0, 42);
        assert!(state.try_ext::<String>().is_none());
        assert!(state.ext::<String>().is_err());
    }
}
