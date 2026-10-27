//! Recoverable Library removal. Media and user Playback have separate lifetimes.
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
        let path = crate::library_paths::resolved_path(path)?;
        self.path_key_in_removal_scope(&path)
    }

    /// Compare a path resolved by the caller before taking the Store lock.
    pub fn path_key_in_removal_scope(&self, path: &Path) -> Result<bool, StoreError> {
        let mut statement = self
            .library
            .prepare("SELECT path FROM library_removal_roots")?;
        for root in statement.query_map([], |row| row.get::<_, String>(0))? {
            if path.starts_with(Path::new(&root?)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(crate) fn ensure_library_mutable(&self, id: &str) -> Result<(), StoreError> {
        if let Some(removal) = self.removal_for_library(id)?
            && removal.state != SUCCEEDED_STATE {
                tracing::error!(library_id = id, operation_id = %removal.operation_id,
                    "拒绝修改冻结的 Library");
                return Err(StoreError::Protected(format!("Library {id} 已冻结")));
            }
        Ok(())
    }

    pub(crate) fn ensure_removal_roots_available(&self, paths: &[&str]) -> Result<(), StoreError> {
        let proposed = paths
            .iter()
            .filter(|path| !path.trim().is_empty())
            .map(|path| crate::library_paths::resolved_path(Path::new(path.trim())))
            .collect::<Result<Vec<_>, _>>()?;
        self.ensure_removal_keys_available(proposed.iter().filter_map(|p| p.to_str()))
    }

    pub(crate) fn ensure_removal_keys_available<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), StoreError> {
        let proposed = keys.into_iter().map(Path::new).collect::<Vec<_>>();
        for removal in self.pending_library_removals()? {
            for root in self.removal_roots(&removal.operation_id)? {
                if proposed
                    .iter()
                    .any(|path| path.starts_with(&root) || root.starts_with(path))
                {
                    tracing::error!(operation_id = %removal.operation_id, root = %root.display(),
                        "拒绝与待删除 Library 重叠的根目录");
                    return Err(StoreError::Protected("根目录与待删除 Library 重叠".into()));
                }
            }
        }
        Ok(())
    }

    /// Freeze before observing any file. The first indexed snapshot window shares
    /// this transaction; later windows cannot race a writer past the persisted barrier.
    pub fn begin_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let existing = tx
            .query_row(
                &format!("SELECT {COLS} FROM library_removals WHERE library_id=?1"),
                [id],
                map_removal,
            )
            .optional()?;
        if let Some(existing) = existing {
            return Ok(existing);
        }
        crate::library_operations::integrity_guard::ensure_no_operation(&tx, id.parse()?)?;
        let (kind, revision): (String, i64) = tx
            .query_row(
                "SELECT kind,config_revision FROM libraries WHERE library_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM libraries l WHERE kind=?1 AND NOT EXISTS
             (SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id)",
            [&kind],
            |row| row.get(0),
        )?;
        if count <= 1 {
            return Err(StoreError::Protected("每种类型的媒体库至少保留一个".into()));
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO library_removals(operation_id,library_id,state,config_revision,
             created_at_ms,updated_at_ms) VALUES(?1,?2,'frozen',?3,?4,?4)",
            params![operation_id, id, revision, crate::unix_now() * 1000],
        )?;
        tx.execute(
            "INSERT INTO library_removal_roots(operation_id,path)
             SELECT ?1,path_key FROM library_roots WHERE library_id=?2",
            params![operation_id, id],
        )?;
        snapshot::capture_batch(&tx, &operation_id)?;
        tx.commit()?;
        tracing::info!(library_id=id,%operation_id,"Library 删除已冻结");
        self.library_removal(&operation_id)?
            .ok_or_else(|| StoreError::Missing(operation_id))
    }

    pub fn removal_for_library(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .query_row(
                &format!("SELECT {COLS} FROM library_removals WHERE library_id=?1"),
                [id],
                map_removal,
            )
            .optional()?)
    }

    pub fn library_removal(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .query_row(
                &format!("SELECT {COLS} FROM library_removals WHERE operation_id=?1"),
                [id],
                map_removal,
            )
            .optional()?)
    }

    /// Removals still owned by the driver (`state != succeeded`), so retention keeps them.
    pub fn pending_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .prepare(&format!(
                "SELECT {COLS} FROM library_removals WHERE state != '{SUCCEEDED_STATE}'"
            ))?
            .query_map([], map_removal)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Boundary roots of one removal operation.
    pub fn removal_roots(&self, id: &str) -> Result<Vec<PathBuf>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT path FROM library_removal_roots WHERE operation_id = ?1 ORDER BY path",
        )?;
        Ok(statement
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }

    /// Removals that already succeeded, so on-disk artifacts a crash left behind
    /// (for example Library cover files) can be reconciled on startup.
    pub fn succeeded_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .prepare(&format!(
                "SELECT {COLS} FROM library_removals WHERE state = '{SUCCEEDED_STATE}'"
            ))?
            .query_map([], map_removal)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn library_accepts_work(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.get_library(id)?.is_some() && self.removal_for_library(id)?.is_none())
    }

    /// Capture at most 100 target files. `true` means the snapshot is complete.
    pub fn capture_library_removal_batch(&self, id: &str) -> Result<bool, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let done = snapshot::capture_batch(&tx, id)?;
        tx.commit()?;
        Ok(done)
    }

    /// One indexed cancellation window; callers release their Store guard between pages.
    pub fn removal_ledger_ids_after(
        &self,
        id: &str,
        after: &str,
    ) -> Result<Vec<String>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT file_id FROM library_removal_files WHERE operation_id=?1 AND file_id>?2
             ORDER BY file_id LIMIT 100",
        )?;
        Ok(statement
            .query_map(params![id, after], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }

    pub fn set_removal_state(
        &self,
        id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        if !matches!(state, "frozen" | "draining" | "cleaning" | "failed") {
            return Err(StoreError::Protected(format!(
                "invalid removal state {state}"
            )));
        }
        let changed = self.library.execute(
            "UPDATE library_removals SET state=?2, error=?3, updated_at_ms=?4
             WHERE operation_id=?1 AND state != 'succeeded'",
            params![id, state, error, crate::unix_now() * 1000],
        )?;
        if changed == 0 {
            return Err(StoreError::Missing(format!("active removal {id}")));
        }
        Ok(())
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
