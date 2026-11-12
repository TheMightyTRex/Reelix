//! Job handlers and scheduled tasks of the radio service.

use std::sync::Arc;
use std::time::Duration;

use reelix_jobs::{JobContext, JobHandler, JobSystemBuilder, TaskDef};
use serde::Deserialize;

use crate::{RadioService, kinds};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IdsPayload {
    show_ids: Vec<i64>,
    /// Queued by a person (API / command): feed shows are refreshed too.
    manual: bool,
}

macro_rules! handler {
    ($name:ident, $kind:expr, |$svc:ident, $p:ident| $body:expr) => {
        pub struct $name(pub Arc<RadioService>);

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

handler!(RefreshRadioShows, kinds::REFRESH_RADIO_SHOWS, |svc, p| {
    let p: IdsPayload = serde_json::from_value(p).unwrap_or_default();
    let ids = (!p.show_ids.is_empty()).then_some(p.show_ids);
    // Scheduled runs leave feeds to the hourly `radio_feed_sync`.
    let skip_feeds = !p.manual && ids.is_none();
    let r = svc.refresh_shows(ids, false, skip_feeds).await?;
    tracing::info!(
        refreshed = r.refreshed,
        failed = r.failed,
        added = r.episodes_added,
        "radio show refresh finished"
    );
    Ok(())
});

handler!(RadioFeedSync, kinds::RADIO_FEED_SYNC, |svc, _p| {
    let r = svc.refresh_shows(None, true, false).await?;
    tracing::info!(
        refreshed = r.refreshed,
        failed = r.failed,
        added = r.episodes_added,
        "radio feed sync finished"
    );
    Ok(())
});

handler!(RadioLibrarySyncJob, kinds::RADIO_LIBRARY_SYNC, |svc, _p| {
    let r = svc.run_library_sync().await?;
    tracing::info!(
        files = r.files_seen,
        removed = r.files_removed,
        matched = r.episodes_matched,
        "radio library sync finished"
    );
    Ok(())
});

handler!(RadioCatchupJob, kinds::RADIO_CATCHUP, |svc, _p| {
    let r = svc.run_catchup().await?;
    if r.tried > 0 {
        tracing::info!(
            tried = r.tried,
            filed = r.filed,
            failed = r.failed,
            "radio catch-up finished"
        );
    }
    Ok(())
});

/// Scheduled tasks: show refresh every 12 h, podcast feeds every hour, the podcast library
/// every 15 minutes, catch-up every hour.
pub fn tasks() -> Vec<TaskDef> {
    vec![
        TaskDef {
            kind: kinds::REFRESH_RADIO_SHOWS,
            name: "Refresh radio shows",
            interval: Duration::from_secs(12 * 3600),
            run_on_startup: false,
        },
        TaskDef {
            kind: kinds::RADIO_FEED_SYNC,
            name: "Sync podcast feeds",
            interval: Duration::from_secs(3600),
            run_on_startup: false,
        },
        TaskDef {
            kind: kinds::RADIO_LIBRARY_SYNC,
            name: "Sync radio library",
            interval: Duration::from_secs(15 * 60),
            run_on_startup: false,
        },
        TaskDef {
            kind: kinds::RADIO_CATCHUP,
            name: "Fetch radio catch-up",
            interval: Duration::from_secs(3600),
            run_on_startup: false,
        },
    ]
}

/// Registers the radio job handlers and [`tasks`].
pub fn register(mut builder: JobSystemBuilder, svc: &Arc<RadioService>) -> JobSystemBuilder {
    builder = builder
        .long_running_handler(RefreshRadioShows(svc.clone()))
        .long_running_handler(RadioFeedSync(svc.clone()))
        .long_running_handler(RadioLibrarySyncJob(svc.clone()))
        .long_running_handler(RadioCatchupJob(svc.clone()));
    for t in tasks() {
        builder = builder.task(t);
    }
    builder
}
