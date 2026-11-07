//! Writes in short transactions. A show with hundreds of episodes (a daily drama has over a
//! thousand) used to be one commit per row on every refresh, numbering and status pass: thousands
//! of disk flushes, during which other writers (API key use, the job runner) waited past the
//! busy timeout and failed with "database is locked". Committing every [`WRITE_BATCH`] rows costs
//! a few flushes, and other writers get the database between batches.

use reelix_db::Db;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use crate::error::Result;

/// Rows per transaction: small enough that another writer waits milliseconds, not seconds.
pub const WRITE_BATCH: usize = 100;

pub struct Batches {
    pool: SqlitePool,
    tx: Option<Transaction<'static, Sqlite>>,
    writes: usize,
}

impl Batches {
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
            tx: None,
            writes: 0,
        }
    }

    /// The open transaction, begun on first use.
    pub async fn conn(&mut self) -> Result<&mut SqliteConnection> {
        if self.tx.is_none() {
            self.tx = Some(self.pool.begin().await?);
        }
        Ok(&mut **self.tx.as_mut().expect("begun above"))
    }

    /// Counts one write; commits when the batch is full.
    pub async fn wrote(&mut self) -> Result<()> {
        self.writes += 1;
        if self.writes.is_multiple_of(WRITE_BATCH) {
            self.commit().await?;
        }
        Ok(())
    }

    /// Commits what is open (nothing when no write was made since the last commit).
    pub async fn commit(&mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit().await?;
        }
        Ok(())
    }
}
