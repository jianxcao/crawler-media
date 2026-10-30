//! Transfer reservation and completion transactions.
//!
//! Prepares and publishes FileCoverage facts during Download transfers,
//! maintaining atomic recovery of pending states without a legacy projection shim.

use std::path::Path;

use domain::{FileCoverage, FilePublication, FileVersion, LedgerId};
use rusqlite::{OptionalExtension, params};

use super::WriteScope;
use super::publish::{
    ensure_config_revision, ensure_library_writable, ensure_media_writable, sync_coverage,
};
use super::reduce::reduce_library_media;
use crate::{Store, StoreError};

impl Store {
    /// Reserve a transfer destination in `Pending` state.
    ///
    /// If an existing pending file already occupies `candidate.file.path`, verifies
    /// identity, source, mode, and returns the existing file identity for recovery.
    pub fn reserve_transfer(
        &self,
        scope: &WriteScope,
        candidate: &FileCoverage,
        source: &Path,
        mode: library::TransferMode,
        _now_ms: i64,
    ) -> Result<FileCoverage, StoreError> {
        self.with_library_write(|db| db.reserve_transfer(scope, candidate, source, mode, _now_ms))
    }

    /// Mark a transferred file as `Published` with the expected version.
    pub fn finish_transfer(
        &self,
        scope: &WriteScope,
        coverage: &FileCoverage,
        expected: &FileVersion,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.finish_transfer(scope, coverage, expected, now_ms))
    }
}

/// Library and Media actually recorded for a file row.
fn persisted_owner(
    tx: &rusqlite::Connection,
    file_id: &str,
) -> Result<(String, String), StoreError> {
    tx.query_row(
        "SELECT library_id, media_id FROM library_files WHERE file_id = ?1",
        params![file_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| StoreError::Missing(format!("file {file_id}")))
}

/// A primary-key hit may only resume the same pending reservation (same
/// Library and destination); it must never re-pend an unrelated row.
fn ensure_same_reservation(
    tx: &rusqlite::Connection,
    file_id: &str,
    scope: &WriteScope,
    dest: &str,
) -> Result<(), StoreError> {
    let same: i64 = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files
                       WHERE file_id = ?1 AND library_id = ?2
                         AND publication_state = 'pending'
                         AND path_key = norm_path_key(?3))",
        params![file_id, scope.library_id.to_string(), dest],
        |r| r.get(0),
    )?;
    if same == 0 {
        return Err(StoreError::Protected(format!(
            "transfer file id {file_id} collides with an unrelated file"
        )));
    }
    Ok(())
}

type ExistingReservation = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn load_reservation(
    conn: &rusqlite::Connection,
    library_id: domain::LibraryId,
    dest_key: &str,
) -> Result<Option<ExistingReservation>, StoreError> {
    conn.query_row(
        "SELECT file_id, media_id, root_id, publication_state, source_path, transfer_mode
                 FROM library_files
                 WHERE library_id = ?1 AND path_key = norm_path_key(?2)",
        params![library_id.to_string(), dest_key],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )
    .optional()
    .map_err(Into::into)
}

#[path = "transfer/connection.rs"]
mod connection;
