//! DB8 Task 3: batched retention for `playback_logs`.
//!
//! Only the library database's playback history is pruned. Watch progress
//! and favorites (`playback_units`), live sessions (`playback_sessions`) and
//! QoE records (`playback_metrics`) are never touched: a row is a candidate
//! only when its own `ended_at` is strictly older than the cutoff.

use rusqlite::params;

use super::{Store, StoreError};

/// Outcome of one [`Store::prune_playback_logs`] batch.
///
/// * `scanned` / `candidates`: expired rows examined in this batch, capped at
///   `batch_size`. Both count the same rows because playback history has no
///   protection rule beyond the age cutoff.
/// * `deleted`: rows actually removed; `0` for `dry_run`.
/// * `protected`: always `0`. Rows inside the retention window are excluded by
///   the candidate predicate and are never scanned, so there is nothing to
///   report; the field exists for parity with the jobs retention batch.
/// * `has_more`: more expired rows remain after this batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackRetentionBatch {
    pub scanned: usize,
    pub candidates: usize,
    pub deleted: usize,
    pub protected: usize,
    pub has_more: bool,
}

/// Read-only preview of what the next [`Store::prune_playback_logs`] batch would
/// see. It never writes: only candidate ids inside the bounded window are read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackRetentionPreview {
    pub scanned: usize,
    pub candidates: usize,
    pub protected: usize,
    /// The fixed server-side budget cut the window short, so the counts are a
    /// lower bound rather than an exact total.
    pub truncated: bool,
}

/// One DELETE statement stays well below SQLite's bound-parameter budget.
const DELETE_CHUNK: usize = 500;

impl Store {
    /// Bounded, read-only candidate count for the maintenance preview endpoint
    /// (DB8 4.2). Uses the same predicate as the real batch, so the numbers mean
    /// the same thing; `truncated` says the budget hid rows instead of pretending
    /// to be an exact total. No def, job or playback row is written.
    pub fn preview_playback_logs(
        &self,
        cutoff: i64,
        batch_size: usize,
    ) -> Result<PlaybackRetentionPreview, StoreError> {
        self.with_library_read(|db| db.preview_playback_logs(cutoff, batch_size))
    }

    /// Delete at most `batch_size` `playback_logs` rows whose `ended_at` is
    /// strictly older than `cutoff`; `ended_at == cutoff` is kept.
    ///
    /// Candidate selection and deletion share one transaction on the library
    /// connection, so a failure leaves the batch untouched and repeated calls
    /// are idempotent. `dry_run` performs the same bounded selection but writes
    /// nothing.
    pub fn prune_playback_logs(
        &self,
        cutoff: i64,
        batch_size: usize,
        dry_run: bool,
    ) -> Result<PlaybackRetentionBatch, StoreError> {
        self.with_library_write(|db| db.prune_playback_logs(cutoff, batch_size, dry_run))
    }
}

#[path = "retention/connection.rs"]
mod connection;
