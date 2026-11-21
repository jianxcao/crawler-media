//! Active rename ownership checks used by presence, scan, publication, and deletion barriers.
use std::path::Path;

use rusqlite::{Connection, params};

use crate::{Store, StoreError};

/// True when `file_id` is currently owned by a non-failed durable rename.
pub(crate) fn has_active_file_rename(conn: &Connection, file_id: &str) -> Result<bool, StoreError> {
    let active: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pending_file_renames WHERE file_id = ?1 AND phase <> 'failed')",
        [file_id],
        |row| row.get(0),
    )?;
    Ok(active)
}

/// True when canonical `path_key` is claimed as source or destination of an active rename,
/// optionally ignoring `exclude_file_id` (the file itself).
pub(crate) fn path_key_claimed_by_active_rename(
    conn: &Connection,
    path_key: &str,
    exclude_file_id: Option<&str>,
) -> Result<bool, StoreError> {
    let claimed: bool = match exclude_file_id {
        Some(file_id) => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_file_renames WHERE phase <> 'failed' AND to_path_key = ?1 AND file_id <> ?2)
                 OR EXISTS(SELECT 1 FROM pending_file_renames WHERE phase <> 'failed' AND from_path_key = ?1 AND file_id <> ?2)",
            params![path_key, file_id],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_file_renames WHERE phase <> 'failed' AND to_path_key = ?1)
                 OR EXISTS(SELECT 1 FROM pending_file_renames WHERE phase <> 'failed' AND from_path_key = ?1)",
            params![path_key],
            |row| row.get(0),
        )?,
    };
    Ok(claimed)
}

impl Store {
    /// True when `path` is claimed as source or destination of an active durable rename.
    pub fn path_claimed_by_active_rename(&self, path: &Path) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.path_claimed_by_active_rename(path))
    }
}

impl crate::LibraryConnection<'_> {
    pub fn path_claimed_by_active_rename(&self, path: &Path) -> Result<bool, StoreError> {
        let key = crate::path_key::canonical_recorded_key(self.library, &path.to_string_lossy());
        path_key_claimed_by_active_rename(self.library, &key, None)
    }
}
