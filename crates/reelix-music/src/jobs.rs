//! Job handlers and scheduled tasks of the music service.

use std::sync::Arc;
use std::time::Duration;

use reelix_jobs::{JobContext, JobHandler, JobSystemBuilder, TaskDef};
use serde::Deserialize;

use crate::{MusicService, kinds};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IdsPayload {
    artist_ids: Vec<i64>,
    manual: bool,
}

fn ids(payload: serde_json::Value) -> IdsPayload {
    serde_json::from_value(payload).unwrap_or_default()
}

macro_rules! handler {
    ($name:ident, $kind:expr, |$svc:ident, $p:ident| $body:expr) => {
        pub struct $name(pub Arc<MusicService>);

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

handler!(RefreshArtists, kinds::REFRESH_ARTISTS, |svc, p| {
    let p = ids(p);
    let failed = svc.refresh_artists(Some(p.artist_ids), p.manual).await?;
    if failed > 0 {
        tracing::warn!(failed, "some artists failed to refresh");
    }
    Ok(())
});

handler!(MusicScan, kinds::MUSIC_SCAN, |svc, p| {
    let p = ids(p);
    let r = svc.scan_artists(&p.artist_ids).await?;
    tracing::info!(
        added = r.added,
        updated = r.updated,
        removed = r.removed,
        "music scan finished"
    );
    Ok(())
});

handler!(MusicRename, kinds::MUSIC_RENAME, |svc, p| {
    let p = ids(p);
    let mut n = 0;
    for id in p.artist_ids {
        n += svc.execute_rename(id).await?;
    }
    tracing::info!(renamed = n, "music rename finished");
    Ok(())
});

/// Scheduled tasks: artist refresh every 24 h (artists refreshed within 7 days are skipped).
pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef {
        kind: kinds::REFRESH_ARTISTS,
        name: "Refresh artists",
        interval: Duration::from_secs(24 * 3600),
        run_on_startup: false,
    }]
}

/// Registers the music job handlers and [`tasks`].
pub fn register(mut builder: JobSystemBuilder, svc: &Arc<MusicService>) -> JobSystemBuilder {
    builder = builder
        .long_running_handler(RefreshArtists(svc.clone()))
        .long_running_handler(MusicScan(svc.clone()))
        .long_running_handler(MusicRename(svc.clone()));
    for t in tasks() {
        builder = builder.task(t);
    }
    builder
}
