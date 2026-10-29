//! Scheduled provider checks: `indexer_health`, `client_health` and `definitions_sync`.

use std::sync::Arc;
use std::time::Duration;

use reelix_jobs::{JobContext, JobHandler, TaskDef};

use crate::ProviderService;

pub const INDEXER_HEALTH: &str = "indexer_health";
pub const CLIENT_HEALTH: &str = "client_health";
pub const DEFINITIONS_SYNC: &str = "definitions_sync";

/// Re-tests failing/disabled indexers and persists their status.
pub struct IndexerHealthJob(pub Arc<ProviderService>);

#[async_trait::async_trait]
impl JobHandler for IndexerHealthJob {
    fn kind(&self) -> &'static str {
        INDEXER_HEALTH
    }

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        self.0.check_indexers().await;
        Ok(())
    }
}

/// Polls download clients (status, warnings, output folders) for the health page.
pub struct ClientHealthJob(pub Arc<ProviderService>);

#[async_trait::async_trait]
impl JobHandler for ClientHealthJob {
    fn kind(&self) -> &'static str {
        CLIENT_HEALTH
    }

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        self.0.check_clients().await;
        Ok(())
    }
}

/// Updates the indexer definition catalog. Payload `{"force": true}` syncs regardless of
/// the configured interval (the "Sync now" button); the hourly task only syncs when due.
pub struct DefinitionsSyncJob(pub Arc<ProviderService>);

#[async_trait::async_trait]
impl JobHandler for DefinitionsSyncJob {
    fn kind(&self) -> &'static str {
        DEFINITIONS_SYNC
    }

    async fn run(&self, _ctx: &JobContext, payload: serde_json::Value) -> anyhow::Result<()> {
        let force = payload
            .get("force")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        // Failures are recorded in the sync status; retrying immediately would not help.
        if let Err(e) = self.0.sync_definitions(force).await {
            tracing::warn!(error = %e, "definitions_sync failed");
        }
        Ok(())
    }
}

/// `indexer_health` every 15 min, `client_health` every 5 min (also on startup),
/// `definitions_sync` every hour (also on startup; syncs only when due).
pub fn tasks() -> Vec<TaskDef> {
    vec![
        TaskDef {
            kind: INDEXER_HEALTH,
            name: "Indexer health check",
            interval: Duration::from_secs(15 * 60),
            run_on_startup: false,
        },
        TaskDef {
            kind: CLIENT_HEALTH,
            name: "Download client health check",
            interval: Duration::from_secs(5 * 60),
            run_on_startup: true,
        },
        TaskDef {
            kind: DEFINITIONS_SYNC,
            name: "Indexer definitions update",
            interval: Duration::from_secs(60 * 60),
            run_on_startup: true,
        },
    ]
}
