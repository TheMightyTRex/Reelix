//! Job handlers and scheduled tasks of the activity service.

use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::pipeline::ManualImportRequest;
use reelix_jobs::{JobContext, JobHandler, JobSystemBuilder, TaskDef};

use super::{ActivityService, kinds};

macro_rules! handler {
    ($name:ident, $kind:expr, |$svc:ident, $p:ident| $body:expr) => {
        pub struct $name(pub Arc<ActivityService>);

        #[async_trait::async_trait]
        impl JobHandler for $name {
            fn kind(&self) -> &'static str {
                $kind
            }

            async fn run(&self, _ctx: &JobContext, payload: serde_json::Value) -> anyhow::Result<()> {
                let $svc = &self.0;
                let $p = payload;
                $body
            }
        }
    };
}

handler!(DownloadMonitor, kinds::DOWNLOAD_MONITOR, |svc, _p| {
    svc.monitor_tick().await?;
    Ok(())
});

handler!(ProcessDownload, kinds::PROCESS_DOWNLOAD, |svc, p| {
    let id = p
        .get("id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("payload needs `id`"))?;
    let imported = svc.process_download(id).await?;
    tracing::info!(id, imported, "download processed");
    Ok(())
});

handler!(ManualImport, kinds::MANUAL_IMPORT, |svc, p| {
    let req: ManualImportRequest = serde_json::from_value(p)?;
    let report = svc.manual_import(&req).await?;
    tracing::info!(
        imported = report.imported,
        failed = report.failures.len(),
        "manual import finished"
    );
    if report.imported == 0 && !report.failures.is_empty() {
        anyhow::bail!("manual import failed: {}", report.failures.join("; "));
    }
    for f in &report.failures {
        tracing::warn!(failure = %f, "manual import: file not imported");
    }
    Ok(())
});

handler!(PruneFinished, kinds::PRUNE_FINISHED, |svc, _p| {
    let n = svc.prune().await?;
    tracing::info!(removed = n, "pruned finished downloads");
    Ok(())
});

/// Scheduled tasks: download monitor (30 s; polls every 2 min when idle) and daily pruning.
pub fn tasks() -> Vec<TaskDef> {
    vec![
        TaskDef {
            kind: kinds::DOWNLOAD_MONITOR,
            name: "Monitor downloads",
            interval: Duration::from_secs(30),
            run_on_startup: true,
        },
        TaskDef {
            kind: kinds::PRUNE_FINISHED,
            name: "Prune finished downloads",
            interval: Duration::from_secs(24 * 3600),
            run_on_startup: false,
        },
    ]
}

/// Registers every activity job handler and scheduled task.
pub fn register(mut builder: JobSystemBuilder, svc: &Arc<ActivityService>) -> JobSystemBuilder {
    builder = builder
        .handler(DownloadMonitor(svc.clone()))
        .handler(ProcessDownload(svc.clone()))
        .handler(ManualImport(svc.clone()))
        .handler(PruneFinished(svc.clone()));
    for t in tasks() {
        builder = builder.task(t);
    }
    builder
}
