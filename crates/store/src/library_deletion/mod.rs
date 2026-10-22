//! Recoverable Library removal. Media and user Playback have separate lifetimes.
//!
//! Two storage decisions matter before changing this module:
//!
//! - `library_removal_roots` is the permanent boundary. A removed root keeps
//!   rejecting work from stale paths after the operation succeeds, and the hot
//!   `path_in_removal_scope` check reads only that small table.
//! - `snapshot_json` exists to recover an interrupted removal (ledger identity
//!   and ownership), so it is pruned to `{"rows":[]}` once the operation succeeds.
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
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

/// Per-file rows of a removal, needed for validation/recovery until it succeeds.
#[derive(Serialize, Deserialize)]
struct Snapshot {
    rows: Vec<RemovalRow>,
}

#[derive(Serialize, Deserialize)]
struct RemovalRow {
    ledger_id: String,
    path: String,
    media_id: String,
    coverage: Vec<(i32, i32)>,
}

const COLS: &str = "operation_id, library_id, state, removed_ledger_count, error";

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
        if let Some(removal) = self.removal_for_library(id)? {
            if removal.state != "succeeded" {
                tracing::error!(library_id = id, operation_id = %removal.operation_id,
                    "拒绝修改冻结的 Library");
                return Err(StoreError::Protected(format!("Library {id} 已冻结")));
            }
        }
        Ok(())
    }

    pub(crate) fn ensure_removal_roots_available(&self, paths: &[&str]) -> Result<(), StoreError> {
        let pending = self.pending_library_removals()?;
        if pending.is_empty() {
            return Ok(());
        }
        let proposed = paths
            .iter()
            .filter(|path| !path.trim().is_empty())
            .map(|path| crate::library_paths::resolved_path(Path::new(path.trim())))
            .collect::<Result<Vec<_>, _>>()?;
        for removal in pending {
            for root in self.removal_roots(&removal.operation_id)? {
                let root = crate::library_paths::resolved_path(&root)?;
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

    pub fn begin_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        if let Some(existing) = self.removal_for_library(id)? {
            return Ok(existing);
        }
        let library = self
            .get_library(id)?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))?;
        let mut count = 0;
        for item in self.list_libraries()? {
            if item.kind == library.kind && self.library_accepts_work(&item.id)? {
                count += 1;
            }
        }
        if count <= 1 {
            return Err(StoreError::Protected("每种类型的媒体库至少保留一个".into()));
        }
        let roots = self
            .roots_of(id)?
            .iter()
            .map(|root| crate::library_paths::resolved_path(root))
            .collect::<Result<Vec<_>, _>>()?;
        let mut rows = Vec::new();
        let mut seen_files = std::collections::HashSet::new();
        for observed in self.list_file_coverage()? {
            let file = &observed.file;
            if !seen_files.insert(file.file_id) {
                continue;
            }
            let Some(media) = self.get_media(file.media_id)? else {
                continue;
            };
            if media.kind != library.kind {
                continue;
            }
            let path = Path::new(&file.path);
            let resolved = crate::library_paths::resolved_path(path)?;
            let under_root = roots
                .iter()
                .map(|root| crate::library_paths::resolved_path(root))
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|root| resolved.starts_with(root));
            if !under_root {
                continue;
            }
            match self.library_for_path_strict(path, media.kind)? {
                Some(owner) if owner.id == id => {
                    let coverage = self
                        .get_file_units(file.file_id)?
                        .into_iter()
                        .map(|unit| (unit.season, unit.episode))
                        .collect();
                    rows.push(RemovalRow {
                        ledger_id: file.file_id.to_string(),
                        path: file.path.to_string_lossy().into_owned(),
                        media_id: media.id.to_string(),
                        coverage,
                    });
                }
                Some(_) => {}
                None => {
                    return Err(StoreError::Protected(
                        "Library 文件归属不唯一，不能安全删除".into(),
                    ));
                }
            }
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        let tx = self.library.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO library_removals (operation_id, library_id, state, snapshot_json,
             removed_ledger_count, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'frozen', ?3, ?4, ?5, ?5)",
            params![
                operation_id,
                id,
                serde_json::to_string(&Snapshot { rows })?,
                0,
                crate::unix_now() * 1000
            ],
        )?;
        for root in &roots {
            tx.execute(
                "INSERT OR IGNORE INTO library_removal_roots (operation_id, path) VALUES (?1, ?2)",
                params![operation_id, root.to_string_lossy()],
            )?;
        }
        tx.commit()?;
        tracing::info!(library_id = id, %operation_id, "Library 删除已冻结");
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

    pub fn pending_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .prepare(&format!(
                "SELECT {COLS} FROM library_removals WHERE state != 'succeeded'"
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
                "SELECT {COLS} FROM library_removals WHERE state = 'succeeded'"
            ))?
            .query_map([], map_removal)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn library_accepts_work(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.get_library(id)?.is_some() && self.removal_for_library(id)?.is_none())
    }

    fn removal_snapshot(&self, id: &str) -> Result<Snapshot, StoreError> {
        let json: String = self.library.query_row(
            "SELECT snapshot_json FROM library_removals WHERE operation_id=?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn removal_ledger_ids(&self, id: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .removal_snapshot(id)?
            .rows
            .into_iter()
            .map(|row| row.ledger_id)
            .collect())
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

    pub fn clean_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        let removal = self
            .library_removal(id)?
            .ok_or_else(|| StoreError::Missing(id.into()))?;
        if removal.state == "succeeded" {
            return Ok(removal);
        }
        if removal.state != "cleaning" {
            return Err(StoreError::Protected("Library 工作尚未排空".into()));
        }
        let snapshot = self.removal_snapshot(id)?;
        self.validate_removal_rows(&snapshot, &removal.library_id)?;
        self.clean_removal_rows(&snapshot, &removal.library_id)
            .map_err(|error| StoreError::Protected(format!("清理 Library 台账失败: {error}")))?;
        #[cfg(test)]
        removal_cleanup_failpoint()?;
        let tx = self.library.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM unidentified WHERE library_id=?1",
            [&removal.library_id],
        )?;
        tx.execute(
            "DELETE FROM library_roots WHERE library_id=?1",
            [&removal.library_id],
        )
        .map_err(|error| StoreError::Protected(format!("删除 Library 根失败: {error}")))?;
        tx.execute(
            "DELETE FROM libraries WHERE library_id=?1",
            [&removal.library_id],
        )
        .map_err(|error| StoreError::Protected(format!("删除 Library 配置失败: {error}")))?;
        tx.execute(
            "UPDATE libraries SET is_default=1 WHERE library_id IN (
            SELECT l.library_id FROM libraries l WHERE NOT EXISTS (
                SELECT 1 FROM libraries d WHERE d.kind=l.kind AND d.is_default=1)
            AND NOT EXISTS (SELECT 1 FROM library_removals r
                WHERE r.library_id=l.library_id AND r.state != 'succeeded')
            AND l.library_id=(SELECT c.library_id FROM libraries c WHERE c.kind=l.kind
                AND NOT EXISTS (SELECT 1 FROM library_removals r
                    WHERE r.library_id=c.library_id AND r.state != 'succeeded')
                ORDER BY c.sort_order, c.library_id LIMIT 1))",
            [],
        )?;
        tx.execute(
            "UPDATE library_removals SET state='succeeded', error=NULL,
            removed_ledger_count=?2, snapshot_json=?3, updated_at_ms=?4 WHERE operation_id=?1",
            params![
                id,
                snapshot.rows.len(),
                serde_json::to_string(&Snapshot { rows: Vec::new() })?,
                crate::unix_now() * 1000
            ],
        )?;
        tx.commit()?;
        for prefix in LIBRARY_SETTING_KEY_PREFIXES {
            let _ = self.delete_setting(&format!("{prefix}{}", removal.library_id));
        }
        tracing::info!(operation_id=id, library_id=%removal.library_id,
            removed_ledger_count=snapshot.rows.len(), "Library 索引删除完成，磁盘文件保留");
        self.library_removal(id)?
            .ok_or_else(|| StoreError::Missing(id.into()))
    }

    fn validate_removal_rows(
        &self,
        snapshot: &Snapshot,
        library_id: &str,
    ) -> Result<(), StoreError> {
        let mut seen_files =
            std::collections::HashMap::<&str, std::collections::BTreeSet<(i32, i32)>>::new();
        for row in &snapshot.rows {
            seen_files
                .entry(row.ledger_id.as_str())
                .or_default()
                .extend(row.coverage.iter().copied());
        }
        for (ledger_id, snapshot_units) in seen_files {
            let first = snapshot
                .rows
                .iter()
                .find(|row| row.ledger_id == ledger_id)
                .ok_or_else(|| StoreError::Protected("删除快照台账身份无效".into()))?;
            let current: Option<(String, String)> = self
                .library
                .query_row(
                    "SELECT display_path, media_id FROM library_files WHERE file_id = ?1",
                    [ledger_id],
                    |record| Ok((record.get::<_, String>(0)?, record.get::<_, String>(1)?)),
                )
                .optional()?;
            // Absent rows are expected after an interrupted library.db commit.
            let Some((path, media_id)) = current else {
                continue;
            };
            let mut stmt = self
                .library
                .prepare("SELECT season, episode FROM library_file_units WHERE file_id = ?1")?;
            let rows = stmt.query_map([ledger_id], |record| {
                Ok((record.get::<_, i32>(0)?, record.get::<_, i32>(1)?))
            })?;
            let current_units: std::collections::BTreeSet<(i32, i32)> =
                rows.collect::<Result<_, _>>()?;
            let unchanged = path == first.path && media_id == first.media_id;
            let media_id = first
                .media_id
                .parse()
                .map_err(|_| StoreError::Protected("删除快照 Media 身份无效".into()))?;
            let owned = match self.get_media(media_id)? {
                Some(media) => self
                    .library_for_path_strict(Path::new(&first.path), media.kind)?
                    .is_some_and(|owner| owner.id == library_id),
                None => false,
            };
            if !unchanged || current_units != snapshot_units || !owned {
                tracing::error!(%library_id, %ledger_id, "删除快照台账身份或归属已改变");
                return Err(StoreError::Protected("删除快照台账身份或归属已改变".into()));
            }
        }
        Ok(())
    }

    fn clean_removal_rows(&self, snapshot: &Snapshot, library_id: &str) -> Result<(), StoreError> {
        let library_id: domain::LibraryId = library_id
            .parse()
            .map_err(|_| StoreError::Protected("删除快照 Library 身份无效".into()))?;
        let file_ids = snapshot
            .rows
            .iter()
            .map(|row| row.ledger_id.parse::<domain::LedgerId>())
            .collect::<Result<std::collections::HashSet<_>, _>>()
            .map_err(|_| StoreError::Protected("删除快照台账身份无效".into()))?
            .into_iter()
            .collect::<Vec<_>>();
        tracing::info!(library_id = %library_id, files = file_ids.len(), "开始清理 Library 台账行");
        // Removing a file must go through the summary-maintaining path: the deferred
        // `preferred_file_id` composite foreign keys from `library_units`/`library_items`
        // point at `library_files` rows, so a bare row delete would abort the commit.
        // Everything stays in this one transaction, so any failure rolls back the
        // whole cleanup instead of leaving a partially reduced Library.
        let tx = self.library.unchecked_transaction()?;
        let removed = crate::library_write::delete::delete_files_in_tx(&tx, library_id, &file_ids)?;
        let mut unit_rows = std::collections::HashSet::new();
        for row in &snapshot.rows {
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
        tx.commit()?;
        tracing::info!(library_id = %library_id, removed, "Library 台账行清理完成");
        Ok(())
    }

    /// Close active playback sessions matching files being removed, preserving last progress.
    /// Sessions for the same Media hosted in another active Library are unaffected.
    pub fn close_playback_sessions_for_removal(&self, id: &str) -> Result<usize, StoreError> {
        let snapshot = self.removal_snapshot(id)?;
        if snapshot.rows.is_empty() {
            return Ok(0);
        }
        let removal_ledger_ids: std::collections::HashSet<&str> =
            snapshot.rows.iter().map(|r| r.ledger_id.as_str()).collect();

        let mut session_stmt = self.library.prepare(
            "SELECT user_id, device_id, media_id, season, episode, file_id FROM playback_sessions",
        )?;
        let sessions = session_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i32>>(3)?,
                    row.get::<_, Option<i32>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut closed = 0;
        let now = crate::unix_now();
        for (user_id_str, device_id, media_id, season, episode, session_ledger_id) in sessions {
            let matches_removal = if let Some(ref lid) = session_ledger_id {
                removal_ledger_ids.contains(lid.as_str())
            } else {
                let in_snapshot = snapshot.rows.iter().any(|row| {
                    row.media_id == media_id
                        && row.coverage.contains(&(
                            season.unwrap_or(crate::UNIT_WHOLE),
                            episode.unwrap_or(crate::UNIT_WHOLE),
                        ))
                });
                if !in_snapshot {
                    false
                } else {
                    let mut ledger_stmt = self.library.prepare(
                        "SELECT u.file_id FROM library_file_units u
                         JOIN library_files f ON f.file_id = u.file_id
                         WHERE u.media_id=?1 AND u.season IS ?2 AND u.episode IS ?3",
                    )?;
                    let mut rows_cursor = ledger_stmt.query(params![media_id, season, episode])?;
                    let mut other_exists = false;
                    while let Some(row) = rows_cursor.next()? {
                        let row_id: String = row.get(0)?;
                        if !removal_ledger_ids.contains(row_id.as_str()) {
                            other_exists = true;
                            break;
                        }
                    }
                    !other_exists
                }
            };

            if !matches_removal {
                continue;
            }

            let user_id = user_id_str
                .parse::<domain::UserId>()
                .map_err(|e| StoreError::Protected(format!("会话用户 ID 无效: {e}")))?;
            if self.close_session(user_id, &device_id, now)?.is_some() {
                tracing::info!(%device_id, %media_id, "已关闭待删除媒体库相关的活跃播放会话");
                closed += 1;
            }
        }
        Ok(closed)
    }
}

/// Delete every `settings` row owned by `library_id`, per the prefix registry.
fn delete_library_scoped_settings(
    conn: &Connection,
    library_id: &str,
) -> Result<usize, StoreError> {
    let mut deleted = 0;
    for prefix in LIBRARY_SETTING_KEY_PREFIXES {
        deleted += conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            [format!("{prefix}{library_id}")],
        )?;
    }
    Ok(deleted)
}

// Thread-local, unit-test-only injection: never exposed through production configuration.
#[cfg(test)]
std::thread_local! {
    static INTERRUPT_AFTER_LIBRARY_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
