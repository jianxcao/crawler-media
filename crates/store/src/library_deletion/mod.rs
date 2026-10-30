//! Recoverable Library removal.
//!
//! Files of the Library are deleted in bounded windows. When a Media loses its
//! last file and nothing user-curated still points at it, it is purged with the
//! rows it owns (same rule as single-file delete). Historic logs stay.
//!
//! Two storage decisions matter before changing this module:
//!
//! - `library_removal_roots` is the permanent boundary. A removed root keeps
//!   rejecting work from stale paths after the operation succeeds, and the hot
//!   `path_in_removal_scope` check reads only that small table.
//! - Indexed removal-file rows preserve the frozen identity in bounded windows.
//!   A restart continues the snapshot/cleanup without decoding the entire Library.
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Store, StoreError};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LibraryRemoval {
    pub operation_id: String,
    pub library_id: String,
    pub state: String,
    pub removed_ledger_count: usize,
    pub error: Option<String>,
}

mod cleanup;
mod sessions;
mod snapshot;

use snapshot::RemovalRow;

const COLS: &str = "operation_id, library_id, state, removed_ledger_count, error";

/// `library_removals.state` of a finished removal. Every other state (`frozen`, `draining`,
/// `cleaning`, `failed`) is still owned by the removal driver and must survive retention.
pub(crate) const SUCCEEDED_STATE: &str = "succeeded";

/// Terminal predicate for retention, derived from the state this module writes.
#[allow(dead_code)]
pub(crate) fn terminal_operation_predicate() -> String {
    format!("state = '{SUCCEEDED_STATE}'")
}

/// `settings` keys owned by a Library, stored as `<prefix><library_id>`.
///
/// Library-scoped settings die with the Library; add the prefix here rather
/// than growing a second cleanup path.
const LIBRARY_SETTING_KEY_PREFIXES: &[&str] = &["library_cover_checked."];

fn map_removal(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryRemoval> {
    Ok(LibraryRemoval {
        operation_id: row.get(0)?,
        library_id: row.get(1)?,
        state: row.get(2)?,
        removed_ledger_count: row.get(3)?,
        error: row.get(4)?,
    })
}

impl Store {
    /// Deleted roots remain a boundary until explicitly reconfigured as a new Library.
    ///
    /// Reads only the small `library_removal_roots` table, so a path check never
    /// deserializes a snapshot that lists every removed file.
    pub fn path_in_removal_scope(&self, path: &Path) -> Result<bool, StoreError> {
        let resolved = crate::library_paths::resolved_path(path)?;
        self.path_key_in_removal_scope(&resolved)
    }

    /// Compare a path resolved by the caller before taking the Store lock.
    pub fn path_key_in_removal_scope(&self, path: &Path) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.path_key_in_removal_scope(path))
    }

    /// Freeze before observing any file. The first indexed snapshot window shares
    /// this transaction; later windows cannot race a writer past the persisted barrier.
    pub fn begin_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        self.with_library_write(|db| db.begin_library_removal(id))
    }

    pub fn removal_for_library(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        self.with_library_read(|db| db.removal_for_library(id))
    }

    pub fn library_removal(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        self.with_library_read(|db| db.library_removal(id))
    }

    /// Public completion includes the independent Subscribe detachment commit.
    /// The durable succeeded receipt still drives recovery between those commits.
    pub fn library_removal_status(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        let Some(mut operation) = self.library_removal(id)? else {
            return Ok(None);
        };
        if operation.state == SUCCEEDED_STATE {
            let bound: bool = self.with_read(crate::Db::Subscribe, |subscribe| {
                Ok(subscribe.query_row(
                    "SELECT EXISTS(SELECT 1 FROM subscribes WHERE library_id=?1)",
                    [&operation.library_id],
                    |row| row.get(0),
                )?)
            })?;
            if bound {
                operation.state = "cleaning".into();
            }
        }
        Ok(Some(operation))
    }

    /// Removals still owned by the driver (`state != succeeded`), so retention keeps them.
    pub fn pending_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        self.with_library_read(|db| db.pending_library_removals())
    }

    /// Boundary roots of one removal operation.
    pub fn removal_roots(&self, id: &str) -> Result<Vec<PathBuf>, StoreError> {
        self.with_library_read(|db| db.removal_roots(id))
    }

    /// Removals that already succeeded, so on-disk artifacts a crash left behind
    /// (for example Library cover files) can be reconciled on startup.
    pub fn succeeded_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        self.with_library_read(|db| db.succeeded_library_removals())
    }

    pub fn library_accepts_work(&self, id: &str) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.library_accepts_work(id))
    }

    /// Capture at most 100 target files. `true` means the snapshot is complete.
    pub fn capture_library_removal_batch(&self, id: &str) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.capture_library_removal_batch(id))
    }

    /// One indexed cancellation window; callers release their Store guard between pages.
    pub fn removal_ledger_ids_after(
        &self,
        id: &str,
        after: &str,
    ) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.removal_ledger_ids_after(id, after))
    }

    pub fn set_removal_state(
        &self,
        id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.set_removal_state(id, state, error))
    }

    /// Standalone convenience driver. API uses `clean_library_removal_batch` and
    /// releases its outer Store guard between calls.
    pub fn clean_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        while !self.capture_library_removal_batch(id)? {}
        loop {
            let operation = self.clean_library_removal_batch(id)?;
            if operation.state == SUCCEEDED_STATE {
                return Ok(operation);
            }
        }
    }

    /// One transaction of at most 100 frozen files, or bounded finalization.
    pub fn clean_library_removal_batch(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        cleanup::advance(self, id)
    }
}

// Thread-local, unit-test-only injection: never exposed through production configuration.
#[cfg(test)]
std::thread_local! {
    static INTERRUPT_AFTER_LIBRARY_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 本批文件覆盖的单元：没有任何剩余文件时，清掉未锁定标记与季对比历史。
fn clean_unit_facts<'a>(
    tx: &rusqlite::Connection,
    rows: impl Iterator<Item = &'a RemovalRow>,
) -> Result<(), StoreError> {
    let mut unit_rows = std::collections::HashSet::new();
    for row in rows {
        for (season, episode) in &row.coverage {
            if !unit_rows.insert((&row.media_id, *season, *episode)) {
                continue;
            }
            tx.execute(
                "DELETE FROM media_markers WHERE media_id=?1 AND season IS ?2 AND episode IS ?3
                 AND locked=0
                 AND NOT EXISTS (
                     SELECT 1 FROM library_file_units u JOIN library_files f ON f.file_id = u.file_id
                     WHERE u.media_id=?1 AND u.season IS ?2 AND u.episode IS ?3)",
                params![&row.media_id, season, episode],
            )?;
            if *season >= 0 {
                tx.execute(
                    "DELETE FROM probe_season_comparison_history WHERE media_id=?1 AND season=?2
                     AND NOT EXISTS (
                         SELECT 1 FROM library_file_units u JOIN library_files f ON f.file_id = u.file_id
                         WHERE u.media_id=?1 AND u.season=?2)",
                    params![&row.media_id, season],
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    static INTERRUPT_AFTER_BATCHES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// 每批提交后调用：计数归零时模拟崩溃，用来证明批次各自持久且可续跑。
#[cfg(test)]
fn removal_batch_failpoint() -> Result<(), StoreError> {
    INTERRUPT_AFTER_BATCHES.with(|slot| match slot.get() {
        Some(0) | Some(1) => {
            slot.set(None);
            Err(StoreError::Protected("simulated batch interruption".into()))
        }
        Some(n) => {
            slot.set(Some(n - 1));
            Ok(())
        }
        None => Ok(()),
    })
}

#[cfg(test)]
fn removal_cleanup_failpoint() -> Result<(), StoreError> {
    if INTERRUPT_AFTER_LIBRARY_COMMIT.with(|flag| flag.replace(false)) {
        return Err(StoreError::Protected(
            "simulated cleanup interruption".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[path = "connection.rs"]
mod connection;
