//! Built-in job handlers and scheduled tasks.

use std::time::Duration;

use reelix_db::now_ms;

use crate::{JobContext, JobHandler, TaskDef};

const WEEK_MS: i64 = 7 * 24 * 3600 * 1000;

/// kind "housekeeping": prune finished jobs older than 7 days, delete expired sessions, `PRAGMA optimize`.
pub struct Housekeeping;

#[async_trait::async_trait]
impl JobHandler for Housekeeping {
    fn kind(&self) -> &'static str {
        "housekeeping"
    }

    async fn run(&self, ctx: &JobContext, _payload: serde_json::Value) -> anyhow::Result<()> {
        let now = now_ms();
        let jobs = ctx.db.jobs().prune_finished(now - WEEK_MS).await?;
        let sessions = ctx.db.sessions().delete_expired(now).await?;
        ctx.db.optimize().await?;
        tracing::info!(
            pruned_jobs = jobs,
            expired_sessions = sessions,
            "housekeeping finished"
        );
        Ok(())
    }
}

/// Default scheduled tasks: housekeeping every 24h (run_on_startup = false).
pub fn default_tasks() -> Vec<TaskDef> {
    vec![TaskDef {
        kind: "housekeeping",
        name: "Housekeeping",
        interval: Duration::from_secs(24 * 3600),
        run_on_startup: false,
    }]
}

#[cfg(test)]
mod tests {
    use reelix_core::bus::EventBus;
    use reelix_db::{Db, NewJob};
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[tokio::test]
    async fn housekeeping_prunes_old_jobs_and_expired_sessions() {
        let db = Db::in_memory().await.unwrap();
        let old = db
            .jobs()
            .enqueue(NewJob::new("x", serde_json::json!({})))
            .await
            .unwrap()
            .unwrap();
        let recent = db
            .jobs()
            .enqueue(NewJob::new("x", serde_json::json!({})))
            .await
            .unwrap()
            .unwrap();
        let pending = db
            .jobs()
            .enqueue(NewJob::new("y", serde_json::json!({})))
            .await
            .unwrap()
            .unwrap();
        db.jobs().claim("x", now_ms()).await.unwrap();
        db.jobs().claim("x", now_ms()).await.unwrap();
        db.jobs().complete(old, now_ms() - WEEK_MS - 1000).await.unwrap();
        db.jobs().complete(recent, now_ms()).await.unwrap();

        let ctx = JobContext {
            db: db.clone(),
            events: EventBus::default(),
            job_id: 0,
            cancel: CancellationToken::new(),
        };
        Housekeeping.run(&ctx, serde_json::json!({})).await.unwrap();

        let ids: Vec<_> = db
            .jobs()
            .recent(10)
            .await
            .unwrap()
            .into_iter()
            .map(|j| j.id)
            .collect();
        assert_eq!(ids, vec![pending, recent]);
    }

    #[test]
    fn default_tasks_include_housekeeping() {
        let tasks = default_tasks();
        assert_eq!(tasks[0].kind, Housekeeping.kind());
        assert!(!tasks[0].run_on_startup);
    }
}
