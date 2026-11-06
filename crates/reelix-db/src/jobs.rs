use chrono::{DateTime, Utc};
use reelix_core::jobs::JobStatus;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Db, Result, decode_err, from_ms, now_ms};

/// A job to enqueue.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: String,
    pub payload: serde_json::Value,
    pub priority: i64,
    pub max_attempts: i64,
    pub run_after_ms: i64,
    pub dedupe_key: Option<String>,
}

impl NewJob {
    /// Priority 0, 3 attempts, runnable now, no dedupe key.
    pub fn new(kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
            priority: 0,
            max_attempts: 3,
            run_after_ms: now_ms(),
            dedupe_key: None,
        }
    }

    /// Skip enqueueing while a queued/running job with the same key exists.
    pub fn dedupe(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    /// Higher runs first.
    pub fn priority(mut self, p: i64) -> Self {
        self.priority = p;
        self
    }
}

/// A persisted job row.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub status: JobStatus,
    pub priority: i64,
    pub attempts: i64,
    pub max_attempts: i64,
    pub run_after: DateTime<Utc>,
    pub dedupe_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

const JOB_COLS: &str = "id, kind, payload, status, priority, attempts, max_attempts, run_after, dedupe_key, \
                        created_at, started_at, finished_at, error";

fn map_job(row: &SqliteRow) -> Result<Job> {
    let status: String = row.try_get("status")?;
    let payload: String = row.try_get("payload")?;
    let started: Option<i64> = row.try_get("started_at")?;
    let finished: Option<i64> = row.try_get("finished_at")?;
    Ok(Job {
        id: row.try_get("id")?,
        kind: row.try_get("kind")?,
        payload: serde_json::from_str(&payload)?,
        status: JobStatus::parse(&status)
            .ok_or_else(|| decode_err(format!("unknown job status {status:?}")))?,
        priority: row.try_get("priority")?,
        attempts: row.try_get("attempts")?,
        max_attempts: row.try_get("max_attempts")?,
        run_after: from_ms(row.try_get("run_after")?),
        dedupe_key: row.try_get("dedupe_key")?,
        created_at: from_ms(row.try_get("created_at")?),
        started_at: started.map(from_ms),
        finished_at: finished.map(from_ms),
        error: row.try_get("error")?,
    })
}

/// The persistent job queue table.
pub struct JobRepo<'a> {
    pub(crate) db: &'a Db,
}

impl JobRepo<'_> {
    /// Inserts a job; `None` if an active (queued/running) job with the same dedupe key exists.
    pub async fn enqueue(&self, job: NewJob) -> Result<Option<i64>> {
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT OR IGNORE INTO jobs (kind, payload, status, priority, attempts, max_attempts, run_after, dedupe_key, created_at)
             VALUES (?, ?, 'queued', ?, 0, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&job.kind)
        .bind(serde_json::to_string(&job.payload)?)
        .bind(job.priority)
        .bind(job.max_attempts.max(1))
        .bind(job.run_after_ms)
        .bind(&job.dedupe_key)
        .bind(now_ms())
        .fetch_optional(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Atomically claims the highest-priority, oldest runnable queued job of `kind`.
    pub async fn claim(&self, kind: &str, now_ms: i64) -> Result<Option<Job>> {
        let row = sqlx::query(&format!(
            "UPDATE jobs SET status = 'running', attempts = attempts + 1, started_at = ?1
             WHERE status = 'queued' AND id = (
                 SELECT id FROM jobs WHERE kind = ?2 AND status = 'queued' AND run_after <= ?1
                 ORDER BY priority DESC, run_after ASC, id ASC LIMIT 1)
             RETURNING {JOB_COLS}"
        ))
        .bind(now_ms)
        .bind(kind)
        .fetch_optional(self.db.pool())
        .await?;
        row.as_ref().map(map_job).transpose()
    }

    pub async fn complete(&self, id: i64, now_ms: i64) -> Result<()> {
        sqlx::query("UPDATE jobs SET status = 'completed', finished_at = ?, error = NULL WHERE id = ?")
            .bind(now_ms)
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Records a failure: `Some(retry_at)` re-queues the job, `None` marks it failed.
    pub async fn fail(&self, id: i64, now_ms: i64, error: &str, retry_at_ms: Option<i64>) -> Result<()> {
        let q = match retry_at_ms {
            Some(at) => {
                sqlx::query("UPDATE jobs SET status = 'queued', run_after = ?, error = ? WHERE id = ?")
                    .bind(at)
            }
            None => sqlx::query("UPDATE jobs SET status = 'failed', finished_at = ?, error = ? WHERE id = ?")
                .bind(now_ms),
        };
        q.bind(error).bind(id).execute(self.db.pool()).await?;
        Ok(())
    }

    /// Crash recovery: puts jobs left `running` by a previous process back in the queue. A job
    /// that was on its last attempt is marked failed instead, so a job that takes the process
    /// down (for example by running out of memory) cannot crash it again on every start.
    pub async fn requeue_running(&self) -> Result<u64> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "UPDATE jobs SET status = 'failed', finished_at = ?, \
             error = 'interrupted on its last attempt (the process stopped while it was running)' \
             WHERE status = 'running' AND attempts >= max_attempts",
        )
        .bind(now_ms())
        .execute(&mut *tx)
        .await?;
        let r = sqlx::query("UPDATE jobs SET status = 'queued' WHERE status = 'running'")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(r.rows_affected())
    }

    /// Newest first.
    pub async fn recent(&self, limit: i64) -> Result<Vec<Job>> {
        let rows = sqlx::query(&format!("SELECT {JOB_COLS} FROM jobs ORDER BY id DESC LIMIT ?"))
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?;
        rows.iter().map(map_job).collect()
    }

    pub async fn get(&self, id: i64) -> Result<Option<Job>> {
        let row = sqlx::query(&format!("SELECT {JOB_COLS} FROM jobs WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        row.as_ref().map(map_job).transpose()
    }

    /// Kinds with at least one queued job that is runnable at `now_ms`. A plain read, so the
    /// runner can skip kinds with nothing to do instead of trying a claim (a write) for each.
    pub async fn ready_kinds(&self, now_ms: i64) -> Result<std::collections::HashSet<String>> {
        let kinds: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT kind FROM jobs WHERE status = 'queued' AND run_after <= ?")
                .bind(now_ms)
                .fetch_all(self.db.pool())
                .await?;
        Ok(kinds.into_iter().collect())
    }

    /// Whether any queued/running job of `kind` exists.
    pub async fn is_active(&self, kind: &str) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = ? AND status IN ('queued', 'running'))",
        )
        .bind(kind)
        .fetch_one(self.db.pool())
        .await?)
    }

    /// Deletes completed/failed jobs that finished before `older_than_ms`.
    pub async fn prune_finished(&self, older_than_ms: i64) -> Result<u64> {
        let r = sqlx::query("DELETE FROM jobs WHERE status IN ('completed', 'failed') AND finished_at < ?")
            .bind(older_than_ms)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn enqueue_and_dedupe() {
        let db = Db::in_memory().await.unwrap();
        let a = db
            .jobs()
            .enqueue(NewJob::new("k", json!({"x": 1})).dedupe("d"))
            .await
            .unwrap();
        assert!(a.is_some());
        assert!(
            db.jobs()
                .enqueue(NewJob::new("k", json!({})).dedupe("d"))
                .await
                .unwrap()
                .is_none()
        );
        // No dedupe key: never deduplicated.
        assert!(
            db.jobs()
                .enqueue(NewJob::new("k", json!({})))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.jobs()
                .enqueue(NewJob::new("k", json!({})))
                .await
                .unwrap()
                .is_some()
        );

        // Still deduped while running; allowed again once finished.
        let job = db.jobs().claim("k", now_ms()).await.unwrap().unwrap();
        assert_eq!(Some(job.id), a);
        assert_eq!(job.payload, json!({"x": 1}));
        assert!(
            db.jobs()
                .enqueue(NewJob::new("k", json!({})).dedupe("d"))
                .await
                .unwrap()
                .is_none()
        );
        db.jobs().complete(job.id, now_ms()).await.unwrap();
        assert!(
            db.jobs()
                .enqueue(NewJob::new("k", json!({})).dedupe("d"))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn claim_respects_kind_priority_and_run_after() {
        let db = Db::in_memory().await.unwrap();
        let now = now_ms();
        let low = db
            .jobs()
            .enqueue(NewJob::new("k", json!({})))
            .await
            .unwrap()
            .unwrap();
        let high = db
            .jobs()
            .enqueue(NewJob::new("k", json!({})).priority(10))
            .await
            .unwrap()
            .unwrap();
        let mut later = NewJob::new("k", json!({})).priority(100);
        later.run_after_ms = now + 60_000;
        db.jobs().enqueue(later).await.unwrap();
        db.jobs().enqueue(NewJob::new("other", json!({}))).await.unwrap();

        let j = db.jobs().claim("k", now_ms()).await.unwrap().unwrap();
        assert_eq!(j.id, high);
        assert_eq!(j.status, JobStatus::Running);
        assert_eq!(j.attempts, 1);
        assert!(j.started_at.is_some());
        assert_eq!(db.jobs().claim("k", now_ms()).await.unwrap().unwrap().id, low);
        assert!(db.jobs().claim("k", now_ms()).await.unwrap().is_none());
        assert!(db.jobs().claim("k", now + 61_000).await.unwrap().is_some());
        assert!(db.jobs().claim("nope", now_ms()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn concurrent_claims_never_double_claim() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("j.db").display()))
            .await
            .unwrap();
        for _ in 0..20 {
            db.jobs().enqueue(NewJob::new("k", json!({}))).await.unwrap();
        }
        let mut handles = Vec::new();
        for _ in 0..8 {
            let db = db.clone();
            handles.push(tokio::spawn(async move {
                let mut got = Vec::new();
                while let Some(j) = db.jobs().claim("k", now_ms()).await.unwrap() {
                    got.push(j.id);
                }
                got
            }));
        }
        let mut all = Vec::new();
        for h in handles {
            all.extend(h.await.unwrap());
        }
        all.sort();
        let len = all.len();
        all.dedup();
        assert_eq!(len, 20);
        assert_eq!(all.len(), 20);
    }

    #[tokio::test]
    async fn fail_retry_then_fail_permanently() {
        let db = Db::in_memory().await.unwrap();
        let id = db
            .jobs()
            .enqueue(NewJob::new("k", json!({})))
            .await
            .unwrap()
            .unwrap();
        let j = db.jobs().claim("k", now_ms()).await.unwrap().unwrap();
        let retry_at = now_ms() + 30_000;
        db.jobs()
            .fail(j.id, now_ms(), "boom", Some(retry_at))
            .await
            .unwrap();
        let j = db.jobs().get(id).await.unwrap().unwrap();
        assert_eq!(j.status, JobStatus::Queued);
        assert_eq!(j.error.as_deref(), Some("boom"));
        assert_eq!(j.run_after.timestamp_millis(), retry_at);
        assert!(db.jobs().claim("k", now_ms()).await.unwrap().is_none());

        let j = db.jobs().claim("k", retry_at).await.unwrap().unwrap();
        assert_eq!(j.attempts, 2);
        db.jobs().fail(j.id, now_ms(), "boom2", None).await.unwrap();
        let j = db.jobs().get(id).await.unwrap().unwrap();
        assert_eq!(j.status, JobStatus::Failed);
        assert_eq!(j.error.as_deref(), Some("boom2"));
        assert!(j.finished_at.is_some());
        assert!(!db.jobs().is_active("k").await.unwrap());
    }

    #[tokio::test]
    async fn requeue_fails_jobs_interrupted_on_their_last_attempt() {
        let db = Db::in_memory().await.unwrap();
        let mut last = NewJob::new("k", json!({}));
        last.max_attempts = 1;
        let last = db.jobs().enqueue(last).await.unwrap().unwrap();
        let again = db
            .jobs()
            .enqueue(NewJob::new("k", json!({})))
            .await
            .unwrap()
            .unwrap();
        db.jobs().claim("k", now_ms()).await.unwrap().unwrap();
        db.jobs().claim("k", now_ms()).await.unwrap().unwrap();

        assert_eq!(db.jobs().requeue_running().await.unwrap(), 1);
        let failed = db.jobs().get(last).await.unwrap().unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert!(failed.finished_at.is_some());
        assert!(failed.error.as_deref().unwrap().contains("last attempt"));
        assert_eq!(
            db.jobs().get(again).await.unwrap().unwrap().status,
            JobStatus::Queued
        );
    }

    #[tokio::test]
    async fn ready_kinds_lists_only_runnable_queued_kinds() {
        let db = Db::in_memory().await.unwrap();
        let now = now_ms();
        assert!(db.jobs().ready_kinds(now).await.unwrap().is_empty());
        db.jobs().enqueue(NewJob::new("a", json!({}))).await.unwrap();
        db.jobs().enqueue(NewJob::new("a", json!({}))).await.unwrap();
        let mut later = NewJob::new("b", json!({}));
        later.run_after_ms = now + 60_000;
        db.jobs().enqueue(later).await.unwrap();
        let ready = db.jobs().ready_kinds(now_ms()).await.unwrap();
        assert_eq!(ready.len(), 1);
        assert!(ready.contains("a"));
        // Running jobs are not ready.
        db.jobs().claim("a", now_ms()).await.unwrap().unwrap();
        db.jobs().claim("a", now_ms()).await.unwrap().unwrap();
        assert!(db.jobs().ready_kinds(now_ms()).await.unwrap().is_empty());
        assert!(db.jobs().ready_kinds(now + 120_000).await.unwrap().contains("b"));
    }

    #[tokio::test]
    async fn requeue_recent_active_and_prune() {
        let db = Db::in_memory().await.unwrap();
        let a = db
            .jobs()
            .enqueue(NewJob::new("k", json!({})))
            .await
            .unwrap()
            .unwrap();
        let b = db
            .jobs()
            .enqueue(NewJob::new("k", json!({})))
            .await
            .unwrap()
            .unwrap();
        assert!(db.jobs().is_active("k").await.unwrap());
        assert!(!db.jobs().is_active("x").await.unwrap());

        db.jobs().claim("k", now_ms()).await.unwrap().unwrap();
        assert_eq!(db.jobs().requeue_running().await.unwrap(), 1);
        assert_eq!(db.jobs().get(a).await.unwrap().unwrap().status, JobStatus::Queued);

        let recent = db.jobs().recent(10).await.unwrap();
        assert_eq!(recent.iter().map(|j| j.id).collect::<Vec<_>>(), vec![b, a]);
        assert_eq!(db.jobs().recent(1).await.unwrap().len(), 1);

        let j = db.jobs().claim("k", now_ms()).await.unwrap().unwrap();
        db.jobs().complete(j.id, 1_000).await.unwrap();
        let j2 = db.jobs().claim("k", now_ms()).await.unwrap().unwrap();
        db.jobs().fail(j2.id, 5_000, "x", None).await.unwrap();
        assert_eq!(db.jobs().prune_finished(2_000).await.unwrap(), 1);
        assert_eq!(db.jobs().prune_finished(10_000).await.unwrap(), 1);
        assert!(db.jobs().recent(10).await.unwrap().is_empty());
    }
}
