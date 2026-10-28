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
        Ok(self.library.query_row(
            "SELECT path,file_id,file_size,file_inode,path_key,deletion_id FROM pending_file_deletions
             WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1))
               AND next_retry_at<=?2 AND attempts<?3",
            params![path,now,MAX_FILE_DELETION_ATTEMPTS],
            |row| Ok(QueuedFileDeletion {
                path:row.get(0)?, file_id:row.get(1)?,
                file_size:row.get::<_,Option<i64>>(2)?.map(|v| v as u64),
                file_inode:row.get::<_,Option<i64>>(3)?.map(|v| v as u64),
                path_key:row.get(4)?,
                deletion_id:row.get(5)?,
            }),
        ).optional()?)
    }

    pub fn file_deletion_pending(&self, path: &str) -> Result<bool, StoreError> {
        Ok(self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_file_deletions
                WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1)))",
            [path],
            |row| row.get(0),
        )?)
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
        let tx = crate::write_tx(&self.library)?;
        let file_id: Option<String> = tx
            .query_row(
                "SELECT file_id FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [path],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(file_id) = &file_id {
            let placing: bool = tx.query_row(
                "SELECT publication_state='pending' FROM library_files WHERE file_id=?1",
                [file_id],
                |row| row.get(0),
            )?;
            let renaming: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_file_renames WHERE file_id=?1)",
                [file_id],
                |row| row.get(0),
            )?;
            if renaming || placing {
                tracing::warn!(
                    file_id,
                    renaming,
                    placing,
                    "拒绝删除仍有文件发布所有者的路径"
                );
                return Err(StoreError::Protected(format!(
                    "file {file_id} has a rename or placement owner"
                )));
            }
        }
        tx.execute(
            "INSERT INTO pending_file_deletions (path, path_key, reason, file_id, file_size, file_inode, deletion_id)
             VALUES (?1, norm_path_key(?1), ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT DO NOTHING",
            params![
                path,
                reason,
                file_id,
                identity.map(|(size, _)| size as i64),
                identity.and_then(|(_, ino)| ino).map(|ino| ino as i64),
                uuid::Uuid::new_v4().to_string(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Review M-3: deletions whose backoff has expired (`next_retry_at <= now`).
    ///
    /// The rows are independent and the caller handles the whole list, so the
    /// order only sets the processing sequence: oldest backoff first. Ordering by
    /// `rowid` here would forbid `pending_deletions_due` (SQLite will not pay for
    /// a sort it can get for free from a scan) and turn the window into a full
    /// scan of the queue; `tests/library_query_plans.rs` pins the plan.
    pub fn list_due_file_deletions(&self, now: i64) -> Result<Vec<QueuedFileDeletion>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path, file_id, file_size, file_inode, path_key, deletion_id FROM pending_file_deletions
             WHERE next_retry_at <= ?1 AND attempts < ?2 ORDER BY next_retry_at",
        )?;
        let rows = stmt.query_map(params![now, MAX_FILE_DELETION_ATTEMPTS], |row| {
            Ok(QueuedFileDeletion {
                path: row.get(0)?,
                path_key: row.get(4)?,
                deletion_id: row.get(5)?,
                file_id: row.get(1)?,
                file_size: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                file_inode: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Reload the exact durable generation without resolving a retired root alias.
    pub fn due_file_deletion_intent(
        &self,
        entry: &QueuedFileDeletion,
        now: i64,
    ) -> Result<Option<QueuedFileDeletion>, StoreError> {
        Ok(self.library.query_row(
            "SELECT path,file_id,file_size,file_inode,path_key,deletion_id FROM pending_file_deletions
             WHERE path_key=?1 AND deletion_id=?2 AND next_retry_at<=?3 AND attempts<?4",
            params![entry.path_key,entry.deletion_id,now,MAX_FILE_DELETION_ATTEMPTS],
            |row| Ok(QueuedFileDeletion {
                path:row.get(0)?, file_id:row.get(1)?,
                file_size:row.get::<_,Option<i64>>(2)?.map(|v|v as u64),
                file_inode:row.get::<_,Option<i64>>(3)?.map(|v|v as u64),
                path_key:row.get(4)?, deletion_id:row.get(5)?,
            }),
        ).optional()?)
    }

    pub fn file_deletion_intent_pending(
        &self,
        entry: &QueuedFileDeletion,
    ) -> Result<bool, StoreError> {
        Ok(self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2)",
            params![entry.path_key,entry.deletion_id], |row| row.get(0),
        )?)
    }

    pub fn file_id_at_deletion_path(
        &self,
        entry: &QueuedFileDeletion,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .library
            .query_row(
                "SELECT file_id FROM library_files WHERE path_key=?1",
                [&entry.path_key],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Drop only this stale generation; a replacement intent and file stay intact.
    pub fn abandon_file_deletion(&self, entry: &QueuedFileDeletion) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2",
            params![entry.path_key, entry.deletion_id],
        )?;
        Ok(())
    }

    /// Review L-10: deletions that exhausted their retries, with last error.
    pub fn list_stuck_file_deletions(&self) -> Result<Vec<(String, Option<String>)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path, last_error FROM pending_file_deletions
             WHERE attempts >= ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([MAX_FILE_DELETION_ATTEMPTS], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Make a queued deletion due now (manual "retry now").
    pub fn reset_file_deletion_backoff(&self, path: &str) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE pending_file_deletions SET next_retry_at = 0, attempts = 0
             WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1))",
            [path],
        )?;
        Ok(())
    }

    /// Current file id at `path`, if any.
    pub fn file_id_at_path(&self, path: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .library
            .query_row(
                "SELECT file_id FROM library_files WHERE path_key = norm_path_key(?1) LIMIT 1",
                [path],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Paths still waiting to be deleted, oldest first.
    pub fn list_pending_file_deletions(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT path FROM pending_file_deletions ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Record a failed attempt with exponential backoff (capped at one day).
    pub fn record_file_deletion_failure(
        &self,
        entry: &QueuedFileDeletion,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE pending_file_deletions
             SET attempts = attempts + 1,
                 next_retry_at = ?3 + MIN(86400, 60 * (1 << MIN(attempts, 10))),
                 last_error = ?4
             WHERE path_key = ?1 AND deletion_id = ?2",
            params![entry.path_key, entry.deletion_id, now, error],
        )?;
        Ok(())
    }

    /// Complete only this generation, atomically removing its file facts and intent.
    /// The persisted key keeps recovery independent of the current root registry.
    pub fn complete_file_deletion(&self, entry: &QueuedFileDeletion) -> Result<(), StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let queued_file_id: Option<Option<String>> = tx
            .query_row(
                "SELECT file_id FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2",
                params![entry.path_key, entry.deletion_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(queued_file_id) = queued_file_id else {
            return Ok(());
        };
        let current: Option<(String, String)> = tx
            .query_row(
                "SELECT file_id,library_id FROM library_files WHERE path_key=?1",
                [&entry.path_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((file_id, library_id)) = current
            && Some(file_id.as_str()) == queued_file_id.as_deref()
        {
            crate::library_write::delete::delete_files_in_tx(
                &tx,
                library_id.parse()?,
                &[file_id.parse()?],
            )?;
        }
        tx.execute(
            "DELETE FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2",
            params![entry.path_key, entry.deletion_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
