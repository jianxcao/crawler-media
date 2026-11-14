//! H3 stalled timeout: track when a pending download last made progress.

use rusqlite::{OptionalExtension, params};

use domain::SubscribeId;

use super::{Store, StoreError};

impl Store {
    /// Record an observed `progress` (0..=1). `last_progress_at` only moves
    /// when progress increased. Returns the time progress was last seen to
    /// change (falls back to `submitted_at`), or `None` for unknown rows.
    pub fn touch_pending_progress(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        progress: f64,
        now: i64,
    ) -> Result<Option<i64>, StoreError> {
        self.with_subscribe_write(|db| {
            db.touch_pending_progress(subscribe_id, enclosure, progress, now)
        })
    }

    /// Release an active pending as `stalled`: it no longer holds its slot and
    /// automatic rounds never pick the enclosure again.
    pub fn mark_pending_stalled(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.mark_pending_stalled(subscribe_id, enclosure))
    }

    /// Review N-1: one more round in which the source could not be probed.
    /// Returns `(attempts, first_seen)` for the row, `None` for unknown rows.
    pub fn note_probe_wait(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        now: i64,
    ) -> Result<Option<(i64, i64)>, StoreError> {
        self.with_subscribe_write(|db| db.note_probe_wait(subscribe_id, enclosure, now))
    }

    /// D-M3: remember the info hash the downloader assigned to a pending row.
    pub fn set_pending_info_hash(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        hash: &str,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.set_pending_info_hash(subscribe_id, enclosure, hash))
    }

    /// D-M3: enclosure → info hash for the Subscribe's active pending rows.
    pub fn load_pending_info_hashes(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        self.with_subscribe_read(|db| db.load_pending_info_hashes(subscribe_id))
    }

    /// F-1: episodes (`"s:e,s:e"`) still to keep once the file list is known;
    /// `None` clears the deferred selection.
    pub fn set_pending_selective(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        missing: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.set_pending_selective(subscribe_id, enclosure, missing))
    }

    /// F-1: enclosure → deferred missing episodes for active pending rows.
    pub fn load_pending_selective(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        self.with_subscribe_read(|db| db.load_pending_selective(subscribe_id))
    }
}

#[path = "pending_progress/connection.rs"]
mod connection;
