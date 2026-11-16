//! Job handlers and scheduled tasks of the books service.

use std::sync::Arc;
use std::time::Duration;

use reelix_jobs::{JobContext, JobHandler, JobSystemBuilder, TaskDef};
use serde::Deserialize;

use crate::{BooksService, kinds};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Payload {
    author_ids: Vec<i64>,
    manual: bool,
    apply_monitor: Option<String>,
}

fn payload(v: serde_json::Value) -> Payload {
    serde_json::from_value(v).unwrap_or_default()
}

macro_rules! handler {
    ($name:ident, $kind:expr, |$svc:ident, $p:ident| $body:expr) => {
        pub struct $name(pub Arc<BooksService>);

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

handler!(RefreshAuthors, kinds::REFRESH_AUTHORS, |svc, p| {
    let p = payload(p);
    let ids = (!p.author_ids.is_empty()).then_some(p.author_ids.clone());
    let failed = svc.refresh_authors(ids, p.manual).await?;
    if failed > 0 {
        tracing::warn!(failed, "some authors failed to refresh");
    }
    // New works may already be on disk.
    svc.scan_authors(&p.author_ids).await?;
    // Narrators and length of books monitored as audiobooks (missing or stale only).
    let looked_up = svc.refresh_audiobooks_of(&p.author_ids).await?;
    if looked_up > 0 {
        tracing::info!(looked_up, "audiobook details refreshed");
    }
    Ok(())
});

handler!(BookScan, kinds::BOOK_SCAN, |svc, p| {
    let p = payload(p);
    let r = svc.scan_authors(&p.author_ids).await?;
    tracing::info!(
        added = r.added,
        updated = r.updated,
        removed = r.removed,
        "book scan finished"
    );
    if let Some(m) = p.apply_monitor.as_deref() {
        for id in &p.author_ids {
            svc.apply_scan_monitor(*id, m).await?;
        }
    }
    Ok(())
});

handler!(BookRename, kinds::BOOK_RENAME, |svc, p| {
    let p = payload(p);
    let mut n = 0;
    for id in p.author_ids {
        n += svc.execute_rename(id).await?;
    }
    tracing::info!(renamed = n, "book rename finished");
    Ok(())
});

/// Recurring tasks: refresh of every author (24h).
pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef {
        kind: kinds::REFRESH_AUTHORS,
        name: "Refresh authors",
        interval: Duration::from_secs(24 * 3600),
        run_on_startup: false,
    }]
}

/// Registers the books job handlers and [`tasks`].
pub fn register(mut builder: JobSystemBuilder, svc: &Arc<BooksService>) -> JobSystemBuilder {
    builder = builder
        .long_running_handler(RefreshAuthors(svc.clone()))
        .long_running_handler(BookScan(svc.clone()))
        .long_running_handler(BookRename(svc.clone()));
    for t in tasks() {
        builder = builder.task(t);
    }
    builder
}
