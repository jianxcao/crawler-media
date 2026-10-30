//! M6: Library files whose physical deletion must be retried. The file row
//! stays until the bytes are really gone.

use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

/// The durable deletion owns this path until it is completed or explicitly
/// abandoned. Source writers check this in their own write transaction.
pub(crate) fn ensure_path_not_pending_deletion(
    conn: &rusqlite::Connection,
    path: &str,
) -> Result<(), StoreError> {
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pending_file_deletions
                WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1)))",
        [path],
        |row| row.get(0),
    )?;
    if pending {
        tracing::warn!(path, "拒绝修改由持久删除意图持有的路径");
        return Err(StoreError::Protected(format!(
            "path {path} has a deletion owner"
        )));
    }
    Ok(())
}

/// Review L-10: after this many failed attempts a deletion is no longer
/// retried automatically; it stays listed (with its last error) for the user.
pub const MAX_FILE_DELETION_ATTEMPTS: i64 = 20;

/// Review M-3: a due deletion together with the identity recorded when it
/// was queued. The file is only deleted while that identity still holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedFileDeletion {
    pub path: String,
    pub path_key: String,
    pub deletion_id: String,
    pub file_id: Option<String>,
    pub file_size: Option<u64>,
    pub file_inode: Option<u64>,
}

#[cfg(unix)]
fn inode_of(meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

#[cfg(not(unix))]
fn inode_of(_meta: &std::fs::Metadata) -> Option<u64> {
    None
}

/// Size and inode of `path` right now (`None` when it cannot be read).
pub fn file_identity(path: &str) -> Option<(u64, Option<u64>)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.len(), inode_of(&meta)))
}

impl Store {
    /// Read exactly one due intent; draining one path must not scan the queue.
    pub fn due_file_deletion(
        &self,
        path: &str,
        now: i64,
    ) -> Result<Option<QueuedFileDeletion>, StoreError> {
        self.with_library_read(|db| db.due_file_deletion(path, now))
    }

    pub fn file_deletion_pending(&self, path: &str) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.file_deletion_pending(path))
    }

    /// Publish an identity captured without the process-wide Store lock. Callers
    /// must revalidate their file version/ownership before accepting the intent.
    /// This stage performs SQL only and never refreshes the disk observation.
    pub fn enqueue_file_deletion_observed(
        &self,
        path: &str,
        reason: &str,
        identity: Option<(u64, Option<u64>)>,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.enqueue_file_deletion_observed(path, reason, identity))
    }

    /// Review M-3: deletions whose backoff has expired (`next_retry_at <= now`).
    ///
    /// The rows are independent and the caller handles the whole list, so the
    /// order only sets the processing sequence: oldest backoff first. Ordering by
    /// `rowid` here would forbid `pending_deletions_due` (SQLite will not pay for
    /// a sort it can get for free from a scan) and turn the window into a full
    /// scan of the queue; `tests/library_query_plans.rs` pins the plan.
    pub fn list_due_file_deletions(&self, now: i64) -> Result<Vec<QueuedFileDeletion>, StoreError> {
        self.with_library_read(|db| db.list_due_file_deletions(now))
    }

    /// Reload the exact durable generation without resolving a retired root alias.
    pub fn due_file_deletion_intent(
        &self,
        entry: &QueuedFileDeletion,
        now: i64,
    ) -> Result<Option<QueuedFileDeletion>, StoreError> {
        self.with_library_read(|db| db.due_file_deletion_intent(entry, now))
    }

    pub fn file_deletion_intent_pending(
        &self,
        entry: &QueuedFileDeletion,
    ) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.file_deletion_intent_pending(entry))
    }

    pub fn file_id_at_deletion_path(
        &self,
        entry: &QueuedFileDeletion,
    ) -> Result<Option<String>, StoreError> {
        self.with_library_read(|db| db.file_id_at_deletion_path(entry))
    }

    /// Drop only this stale generation; a replacement intent and file stay intact.
    pub fn abandon_file_deletion(&self, entry: &QueuedFileDeletion) -> Result<(), StoreError> {
        self.with_library_write(|db| db.abandon_file_deletion(entry))
    }

    /// Review L-10: deletions that exhausted their retries, with last error.
    pub fn list_stuck_file_deletions(&self) -> Result<Vec<(String, Option<String>)>, StoreError> {
        self.with_library_read(|db| db.list_stuck_file_deletions())
    }

    /// Make a queued deletion due now (manual "retry now").
    pub fn reset_file_deletion_backoff(&self, path: &str) -> Result<(), StoreError> {
        self.with_library_write(|db| db.reset_file_deletion_backoff(path))
    }

    /// Current file id at `path`, if any.
    pub fn file_id_at_path(&self, path: &str) -> Result<Option<String>, StoreError> {
        self.with_library_read(|db| db.file_id_at_path(path))
    }

    /// Paths still waiting to be deleted, oldest first.
    pub fn list_pending_file_deletions(&self) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.list_pending_file_deletions())
    }

    /// Record a failed attempt with exponential backoff (capped at one day).
    pub fn record_file_deletion_failure(
        &self,
        entry: &QueuedFileDeletion,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.record_file_deletion_failure(entry, now, error))
    }

    /// Complete only this generation, atomically removing its file facts and intent.
    /// The persisted key keeps recovery independent of the current root registry.
    pub fn complete_file_deletion(&self, entry: &QueuedFileDeletion) -> Result<(), StoreError> {
        self.with_reference_write(|library, subscribe| {
            library.complete_file_deletion_with_references(subscribe, entry)
        })
    }
}

#[path = "pending_file_deletions/connection.rs"]
mod connection;
