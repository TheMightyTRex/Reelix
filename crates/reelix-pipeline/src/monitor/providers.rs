//! What the activity service needs from the providers (download clients, path mappings,
//! notifications, media servers), behind a trait so tests can use fakes.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use reelix_core::ContentKind;
use reelix_downloaders::DownloadClient;
use reelix_notify::NotificationEvent;
use reelix_providers::ProviderService;

/// An enabled download client.
#[derive(Clone)]
pub struct ClientHandle {
    pub id: i64,
    pub name: String,
    /// `host` setting (remote path mappings are keyed by it).
    pub host: String,
    pub client: Arc<dyn DownloadClient>,
}

impl std::fmt::Debug for ClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientHandle")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait ActivityProviders: Send + Sync {
    /// Enabled download clients.
    fn clients(&self) -> Vec<ClientHandle>;

    fn client(&self, id: i64) -> Option<ClientHandle> {
        self.clients().into_iter().find(|c| c.id == id)
    }

    /// Download-client path → local path (unchanged when no mapping applies).
    fn map_remote_path(&self, host: &str, remote: &str) -> PathBuf;

    /// Local folders manual import may read from besides root folders: download client
    /// output folders and remote path mapping targets.
    async fn download_roots(&self) -> Vec<PathBuf>;

    /// Fan-out notification; never fails.
    async fn notify(&self, event: &NotificationEvent);

    /// Asks every enabled movie/TV media server to rescan `paths`; errors are logged. Book and
    /// music servers are refreshed by the server's library refresher on import events.
    async fn refresh_media_servers(&self, paths: &[PathBuf]);
}

#[async_trait]
impl ActivityProviders for ProviderService {
    fn clients(&self) -> Vec<ClientHandle> {
        self.download_clients()
            .into_iter()
            .filter(|(row, _)| row.enabled)
            .map(|(row, client)| ClientHandle {
                id: row.id,
                host: row.setting_str("host").unwrap_or_default().to_owned(),
                name: row.name,
                client,
            })
            .collect()
    }

    fn map_remote_path(&self, host: &str, remote: &str) -> PathBuf {
        ProviderService::map_remote_path(self, host, remote)
    }

    async fn download_roots(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .remote_path_mappings()
            .into_iter()
            .map(|m| PathBuf::from(m.local_path))
            .collect();
        for c in ActivityProviders::clients(self) {
            let status = tokio::time::timeout(std::time::Duration::from_secs(15), c.client.status()).await;
            if let Ok(Ok(s)) = status {
                for d in s.output_dirs {
                    out.push(ProviderService::map_remote_path(self, &c.host, &d.path));
                }
            }
        }
        out
    }

    async fn notify(&self, event: &NotificationEvent) {
        ProviderService::notify(self, event).await;
    }

    async fn refresh_media_servers(&self, paths: &[PathBuf]) {
        for (row, server) in self.media_servers() {
            let video = server
                .content_kinds()
                .iter()
                .any(|k| matches!(k, ContentKind::Movie | ContentKind::Series));
            if !row.enabled || !video {
                continue;
            }
            if let Err(e) = server.refresh_library(paths).await {
                tracing::warn!(server = %row.name, error = %e, "media server refresh failed");
            }
        }
    }
}

/// Fixed client list without path mappings, notifications or media servers (tests, tools).
#[derive(Default, Clone)]
pub struct StaticProviders {
    pub clients: Vec<ClientHandle>,
    /// Returned by `download_roots`.
    pub roots: Vec<PathBuf>,
    /// Notifications sent (for assertions).
    pub sent: Arc<std::sync::Mutex<Vec<NotificationEvent>>>,
}

#[async_trait]
impl ActivityProviders for StaticProviders {
    fn clients(&self) -> Vec<ClientHandle> {
        self.clients.clone()
    }

    fn map_remote_path(&self, _host: &str, remote: &str) -> PathBuf {
        PathBuf::from(remote)
    }

    async fn download_roots(&self) -> Vec<PathBuf> {
        self.roots.clone()
    }

    async fn notify(&self, event: &NotificationEvent) {
        self.sent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event.clone());
    }

    async fn refresh_media_servers(&self, _paths: &[PathBuf]) {}
}
