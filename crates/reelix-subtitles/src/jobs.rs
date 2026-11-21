//! Job handler and scheduled task: `subtitles_search` searches one file (payload
//! `{"kind": "movie" | "episode", "file_id": n}`) or, with an empty payload, runs the schedule.

use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::subtitles::SubtitleMediaKind;
use reelix_jobs::{JobContext, JobHandler, JobSystemBuilder, TaskDef};

use crate::error::Error;
use crate::{SubtitleService, kinds};

pub struct SubtitleSearch(pub Arc<SubtitleService>);

#[async_trait::async_trait]
impl JobHandler for SubtitleSearch {
    fn kind(&self) -> &'static str {
        kinds::SEARCH
    }

    async fn run(&self, _ctx: &JobContext, payload: serde_json::Value) -> anyhow::Result<()> {
        let file = payload
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .and_then(SubtitleMediaKind::parse)
            .zip(payload.get("file_id").and_then(serde_json::Value::as_i64));
        match file {
            Some((kind, id)) => match self.0.search_file(kind, id).await {
                Ok(r) => {
                    tracing::info!(
                        kind = kind.as_str(),
                        id,
                        saved = r.saved.len(),
                        missing = r.missing.len(),
                        rejected = r.rejected.len(),
                        "subtitle search finished"
                    );
                    Ok(())
                }
                // Nothing to retry: the allowance or the off switch will not change in minutes.
                Err(Error::Off | Error::QuotaExhausted) => Ok(()),
                Err(e) => Err(e.into()),
            },
            None => {
                let n = self.0.run_due().await?;
                if n > 0 {
                    tracing::info!(files = n, "subtitle run finished");
                }
                Ok(())
            }
        }
    }
}

/// Every 6 hours; each run searches at most 50 files (sources allow little more in a day).
pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef {
        kind: kinds::SEARCH,
        name: "Search for missing subtitles",
        interval: Duration::from_secs(6 * 3600),
        run_on_startup: false,
    }]
}

pub fn register(mut builder: JobSystemBuilder, svc: &Arc<SubtitleService>) -> JobSystemBuilder {
    builder = builder.handler(SubtitleSearch(svc.clone()));
    for t in tasks() {
        builder = builder.task(t);
    }
    builder
}
