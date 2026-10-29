//! Job handlers: `search` (payload [`SearchRequest`]), `rss_sync` and `process_pending`.

use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::library::SearchRequest;
use reelix_jobs::{JobContext, JobHandler, JobSystemBuilder, TaskDef};

use super::SearchService;

pub const SEARCH: &str = "search";
pub const RSS_SYNC: &str = "rss_sync";
pub const PROCESS_PENDING: &str = "process_pending";

pub struct SearchJob(pub Arc<SearchService>);

#[async_trait::async_trait]
impl JobHandler for SearchJob {
    fn kind(&self) -> &'static str {
        SEARCH
    }

    async fn run(&self, ctx: &JobContext, payload: serde_json::Value) -> anyhow::Result<()> {
        let req: SearchRequest = serde_json::from_value(payload)?;
        let cancel = ctx.cancel.clone();
        let stop = move || cancel.is_cancelled();
        self.0.run_search(&req, &stop).await?;
        Ok(())
    }
}

/// Ticks every minute; syncs when the configured RSS interval has elapsed.
pub struct RssSyncJob(pub Arc<SearchService>);

#[async_trait::async_trait]
impl JobHandler for RssSyncJob {
    fn kind(&self) -> &'static str {
        RSS_SYNC
    }

    async fn run(&self, _ctx: &JobContext, payload: serde_json::Value) -> anyhow::Result<()> {
        let force = payload
            .get("force")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        self.0.rss_sync(force).await?;
        Ok(())
    }
}

pub struct ProcessPendingJob(pub Arc<SearchService>);

#[async_trait::async_trait]
impl JobHandler for ProcessPendingJob {
    fn kind(&self) -> &'static str {
        PROCESS_PENDING
    }

    async fn run(&self, _ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        self.0.process_pending().await?;
        Ok(())
    }
}

/// `rss_sync` (1 min tick, interval from settings) and `process_pending` (1 min).
pub fn tasks() -> Vec<TaskDef> {
    vec![
        TaskDef {
            kind: RSS_SYNC,
            name: "RSS sync",
            interval: Duration::from_secs(60),
            run_on_startup: false,
        },
        TaskDef {
            kind: PROCESS_PENDING,
            name: "Process delayed releases",
            interval: Duration::from_secs(60),
            run_on_startup: false,
        },
    ]
}

/// Registers the handlers and tasks.
pub fn register(mut builder: JobSystemBuilder, svc: &Arc<SearchService>) -> JobSystemBuilder {
    builder = builder
        .handler(SearchJob(svc.clone()))
        .handler(RssSyncJob(svc.clone()))
        .handler(ProcessPendingJob(svc.clone()));
    for t in tasks() {
        builder = builder.task(t);
    }
    builder
}
