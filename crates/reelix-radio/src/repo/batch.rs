//! Writes in short transactions, with room for other writers between them. A show with hundreds
//! of episodes (a daily drama has over a thousand) used to be one commit per row on every
//! refresh, numbering and status pass: thousands of disk flushes, during which other writers (API
//! key use, the job runner) waited past the busy timeout and failed with "database is locked".
//!
//! A batch is committed after [`WRITE_BATCH`] rows or [`BATCH_TIME`], whichever comes first, and
//! the next batch waits [`BATCH_GAP`] (RFX-71). The gap matters as much as the short batch: a
//! writer that finds the database locked is retried by SQLite's busy handler, which sleeps up to
//! 100 ms between tries. Batches committed back to back left it only microseconds to get in, so it
//! could wait for most of a long refresh. A gap longer than the busy handler's sleep means a
//! waiting writer gets the database before the next batch.

use std::time::{Duration, Instant};

use reelix_db::Db;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use crate::error::Result;

/// Rows per transaction at most.
pub const WRITE_BATCH: usize = 500;
/// How long one transaction may hold the write lock before it is committed, so on a slow disk a
/// batch ends before its rows are all written.
pub const BATCH_TIME: Duration = Duration::from_millis(50);
/// The pause between batches: longer than the 100 ms the busy handler sleeps between tries.
pub const BATCH_GAP: Duration = Duration::from_millis(120);

pub struct Batches {
    pool: SqlitePool,
    tx: Option<(Transaction<'static, Sqlite>, Instant)>,
    rows: usize,
}

impl Batches {
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
            tx: None,
            rows: 0,
        }
    }

    /// The open transaction, begun on first use.
    pub async fn conn(&mut self) -> Result<&mut SqliteConnection> {
        if self.tx.is_none() {
            self.tx = Some((self.pool.begin().await?, Instant::now()));
        }
        Ok(&mut *self.tx.as_mut().expect("begun above").0)
    }

    /// Counts one write; when the batch is full or has held the lock for [`BATCH_TIME`], commits
    /// it and waits [`BATCH_GAP`] so other writers can get in.
    pub async fn wrote(&mut self) -> Result<()> {
        self.rows += 1;
        let long = self
            .tx
            .as_ref()
            .is_some_and(|(_, began)| began.elapsed() >= BATCH_TIME);
        if self.rows >= WRITE_BATCH || long {
            self.commit().await?;
            tokio::time::sleep(BATCH_GAP).await;
        }
        Ok(())
    }

    /// Commits what is open (nothing when no write was made since the last commit).
    pub async fn commit(&mut self) -> Result<()> {
        self.rows = 0;
        if let Some((tx, _)) = self.tx.take() {
            tx.commit().await?;
        }
        Ok(())
    }
}
