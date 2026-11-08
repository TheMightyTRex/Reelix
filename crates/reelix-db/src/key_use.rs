//! API key use, kept in memory and written to the database at most once a minute.
//!
//! Dashboards, Torznab and *arr clients call Reelix with a key every few seconds. When each of
//! those requests wrote `last_used_at`, a request that only reads still needed the write lock;
//! while a long job held it (a big radio refresh, RFX-71), the request waited past SQLite's busy
//! timeout and failed with "database is locked". A request now notes the time in memory, and
//! [`Db::flush_key_use`] writes every key's latest use in one short transaction. The server calls
//! it every [`KEY_USE_FLUSH_EVERY`] and once more on shutdown, so each key is written at most once a
//! minute, and never on a request's path.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::{Db, Result};

/// How often the server writes key use: at most one write per key in this time.
pub const KEY_USE_FLUSH_EVERY: Duration = Duration::from_secs(60);

/// Latest use of each key not yet written, by key hash.
#[derive(Debug, Default)]
pub(crate) struct KeyUse {
    pending: Mutex<HashMap<String, i64>>,
}

impl KeyUse {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, i64>> {
        // A panic while the map was held leaves it usable: it only holds times.
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Notes a use of the key at `at` (unix milliseconds); keeps the latest.
    pub(crate) fn record(&self, key_hash: &str, at: i64) {
        let mut pending = self.lock();
        match pending.get_mut(key_hash) {
            Some(t) => *t = (*t).max(at),
            None => {
                pending.insert(key_hash.to_owned(), at);
            }
        }
    }

    /// The use not yet written, if any.
    pub(crate) fn pending(&self, key_hash: &str) -> Option<i64> {
        self.lock().get(key_hash).copied()
    }

    /// Forgets a key (it was deleted).
    pub(crate) fn forget(&self, key_hash: &str) {
        self.lock().remove(key_hash);
    }

    fn take(&self) -> HashMap<String, i64> {
        std::mem::take(&mut *self.lock())
    }

    /// Puts back uses a failed flush did not write, keeping any newer one.
    fn restore(&self, uses: HashMap<String, i64>) {
        for (hash, at) in uses {
            self.record(&hash, at);
        }
    }
}

impl Db {
    /// Writes the key use noted since the last flush, in one short transaction; returns how many
    /// keys were written. When the write fails (the database is busy), the uses are kept for the
    /// next flush.
    pub async fn flush_key_use(&self) -> Result<usize> {
        let uses = self.key_use.take();
        if uses.is_empty() {
            return Ok(0);
        }
        match write_uses(self, &uses).await {
            Ok(()) => Ok(uses.len()),
            Err(e) => {
                self.key_use.restore(uses);
                Err(e)
            }
        }
    }
}

async fn write_uses(db: &Db, uses: &HashMap<String, i64>) -> Result<()> {
    let mut tx = db.pool().begin().await?;
    for (hash, at) in uses {
        // Never moves a stamp back (two servers on one file, or a clock change).
        sqlx::query(
            "UPDATE api_keys SET last_used_at = MAX(COALESCE(last_used_at, 0), ?) WHERE key_hash = ?",
        )
        .bind(at)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
