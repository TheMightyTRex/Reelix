use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Db, Result, from_ms};

/// A recurring task row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTask {
    pub kind: String,
    pub name: String,
    pub interval_secs: i64,
    pub enabled: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub last_duration_ms: Option<i64>,
    pub last_error: Option<String>,
}

const TASK_COLS: &str =
    "kind, name, interval_secs, enabled, last_run, next_run, last_duration_ms, last_error";

fn map_task(row: &SqliteRow) -> Result<ScheduledTask> {
    let last: Option<i64> = row.try_get("last_run")?;
    let next: Option<i64> = row.try_get("next_run")?;
    Ok(ScheduledTask {
        kind: row.try_get("kind")?,
        name: row.try_get("name")?,
        interval_secs: row.try_get("interval_secs")?,
        enabled: row.try_get("enabled")?,
        last_run: last.map(from_ms),
        next_run: next.map(from_ms),
        last_duration_ms: row.try_get("last_duration_ms")?,
        last_error: row.try_get("last_error")?,
    })
}

/// Scheduled (recurring) task definitions and their run history.
pub struct TaskRepo<'a> {
    pub(crate) db: &'a Db,
}

impl TaskRepo<'_> {
    /// Inserts the task if missing; an existing row only gets its name updated.
    pub async fn ensure(&self, kind: &str, name: &str, interval_secs: i64, first_run_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO scheduled_tasks (kind, name, interval_secs, enabled, next_run) VALUES (?, ?, ?, 1, ?)
             ON CONFLICT(kind) DO UPDATE SET name = excluded.name",
        )
        .bind(kind)
        .bind(name)
        .bind(interval_secs)
        .bind(first_run_ms)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<ScheduledTask>> {
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLS} FROM scheduled_tasks ORDER BY name, kind"
        ))
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(map_task).collect()
    }

    pub async fn get(&self, kind: &str) -> Result<Option<ScheduledTask>> {
        let row = sqlx::query(&format!("SELECT {TASK_COLS} FROM scheduled_tasks WHERE kind = ?"))
            .bind(kind)
            .fetch_optional(self.db.pool())
            .await?;
        row.as_ref().map(map_task).transpose()
    }

    /// Enabled tasks whose `next_run <= now`.
    pub async fn due(&self, now_ms: i64) -> Result<Vec<ScheduledTask>> {
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLS} FROM scheduled_tasks WHERE enabled = 1 AND next_run IS NOT NULL AND next_run <= ?
             ORDER BY next_run"
        ))
        .bind(now_ms)
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(map_task).collect()
    }

    pub async fn schedule_next(&self, kind: &str, next_run_ms: i64) -> Result<()> {
        sqlx::query("UPDATE scheduled_tasks SET next_run = ? WHERE kind = ?")
            .bind(next_run_ms)
            .bind(kind)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Records a finished run; no-op if `kind` isn't a task.
    pub async fn record_run(
        &self,
        kind: &str,
        finished_ms: i64,
        duration_ms: i64,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE scheduled_tasks SET last_run = ?, last_duration_ms = ?, last_error = ? WHERE kind = ?",
        )
        .bind(finished_ms)
        .bind(duration_ms)
        .bind(error)
        .bind(kind)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::Db;

    #[tokio::test]
    async fn ensure_due_schedule_record() {
        let db = Db::in_memory().await.unwrap();
        db.tasks().ensure("a", "Task A", 60, 1_000).await.unwrap();
        db.tasks().ensure("b", "Task B", 3600, 5_000).await.unwrap();
        // Re-ensure: name updated, interval and schedule kept.
        db.tasks().ensure("a", "Renamed", 999, 99_999).await.unwrap();
        let a = db.tasks().get("a").await.unwrap().unwrap();
        assert_eq!(a.name, "Renamed");
        assert_eq!(a.interval_secs, 60);
        assert!(a.enabled);
        assert_eq!(a.next_run.unwrap().timestamp_millis(), 1_000);
        assert!(a.last_run.is_none());
        assert_eq!(db.tasks().list().await.unwrap().len(), 2);
        assert!(db.tasks().get("zzz").await.unwrap().is_none());

        let due: Vec<_> = db
            .tasks()
            .due(2_000)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect();
        assert_eq!(due, vec!["a"]);
        assert_eq!(db.tasks().due(10_000).await.unwrap().len(), 2);

        db.tasks().schedule_next("a", 50_000).await.unwrap();
        assert!(db.tasks().due(2_000).await.unwrap().is_empty());

        sqlx::query("UPDATE scheduled_tasks SET enabled = 0 WHERE kind = 'b'")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(db.tasks().due(10_000).await.unwrap().is_empty());

        db.tasks().record_run("a", 7_000, 42, Some("oops")).await.unwrap();
        let a = db.tasks().get("a").await.unwrap().unwrap();
        assert_eq!(a.last_run.unwrap().timestamp_millis(), 7_000);
        assert_eq!(a.last_duration_ms, Some(42));
        assert_eq!(a.last_error.as_deref(), Some("oops"));
        db.tasks().record_run("a", 8_000, 1, None).await.unwrap();
        assert!(db.tasks().get("a").await.unwrap().unwrap().last_error.is_none());
        // Unknown kind: no-op.
        db.tasks().record_run("nope", 1, 1, None).await.unwrap();
    }
}
