//! Job handlers and scheduled tasks of the library.

use std::sync::Arc;
use std::time::Duration;

use reelix_api_types::library::RenameRequest;
use reelix_jobs::{JobContext, JobHandler, JobSystemBuilder, TaskDef};
use serde::Deserialize;

use crate::{LibraryService, kinds};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IdsPayload {
    movie_ids: Vec<i64>,
    series_ids: Vec<i64>,
    collection_ids: Vec<i64>,
    manual: bool,
    search_missing: bool,
}

fn ids(payload: serde_json::Value) -> IdsPayload {
    serde_json::from_value(payload).unwrap_or_default()
}

macro_rules! handler {
    ($name:ident, $kind:expr, |$svc:ident, $p:ident| $body:expr) => {
        pub struct $name(pub Arc<LibraryService>);

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

handler!(RefreshMovies, kinds::REFRESH_MOVIES, |svc, p| {
    let p = ids(p);
    let failed = svc.refresh_movies(Some(p.movie_ids), p.manual).await?;
    if failed > 0 {
        tracing::warn!(failed, "some movies failed to refresh");
    }
    Ok(())
});

handler!(RefreshSeries, kinds::REFRESH_SERIES, |svc, p| {
    let p = ids(p);
    let failed = svc.refresh_all_series(Some(p.series_ids), p.manual).await?;
    if failed > 0 {
        tracing::warn!(failed, "some series failed to refresh");
    }
    Ok(())
});

handler!(LibraryScan, kinds::LIBRARY_SCAN, |svc, p| {
    let p = ids(p);
    let r = svc.scan_library(&p.movie_ids, &p.series_ids).await?;
    tracing::info!(
        added = r.added,
        updated = r.updated,
        removed = r.removed,
        "library scan finished"
    );
    if p.search_missing {
        svc.search_missing_of(&p.movie_ids, &p.series_ids).await?;
    }
    Ok(())
});

handler!(RenameFiles, kinds::RENAME_FILES, |svc, p| {
    let req: RenameRequest = serde_json::from_value(p)?;
    let n = svc.execute_rename(&req).await?;
    tracing::info!(renamed = n, "rename finished");
    Ok(())
});

handler!(CollectionSync, kinds::COLLECTION_SYNC, |svc, p| {
    let p = ids(p);
    let added = svc.sync_collections(Some(p.collection_ids)).await?;
    tracing::info!(added, "collection sync finished");
    Ok(())
});

/// Scheduled tasks: movie refresh (24h), series refresh (12h), collection sync (24h).
pub fn tasks() -> Vec<TaskDef> {
    vec![
        TaskDef {
            kind: kinds::REFRESH_MOVIES,
            name: "Refresh movies",
            interval: Duration::from_secs(24 * 3600),
            run_on_startup: false,
        },
        TaskDef {
            kind: kinds::REFRESH_SERIES,
            name: "Refresh series",
            interval: Duration::from_secs(12 * 3600),
            run_on_startup: false,
        },
        TaskDef {
            kind: kinds::COLLECTION_SYNC,
            name: "Sync collections",
            interval: Duration::from_secs(24 * 3600),
            run_on_startup: false,
        },
    ]
}

/// Registers every library job handler and scheduled task.
pub fn register(mut builder: JobSystemBuilder, svc: &Arc<LibraryService>) -> JobSystemBuilder {
    builder = builder
        .handler(RefreshMovies(svc.clone()))
        .handler(RefreshSeries(svc.clone()))
        .handler(LibraryScan(svc.clone()))
        .handler(RenameFiles(svc.clone()))
        .handler(CollectionSync(svc.clone()));
    for t in tasks() {
        builder = builder.task(t);
    }
    builder
}
