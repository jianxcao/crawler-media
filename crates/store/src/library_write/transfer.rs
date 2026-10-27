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
        let tx = crate::write_tx(&self.library)?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;
        ensure_media_writable(&tx, candidate.file.media_id)?;
        if candidate.file.library_id != scope.library_id {
            return Err(StoreError::Protected(format!(
                "transfer candidate library {} is outside write scope {}",
                candidate.file.library_id, scope.library_id
            )));
        }

        // Root-relative destinations resolve once, here; keys are always absolute.
        let absolutized =
            super::root_ownership::ensure_root_owns_path(&tx, &candidate.file)?.map(|file| {
                FileCoverage {
                    path_key: candidate.path_key.clone(),
                    file,
                    units: candidate.units.clone(),
                }
            });
        let candidate = absolutized.as_ref().unwrap_or(candidate);
        let dest_path_str = candidate.file.path.to_string_lossy().to_string();
        let dest_key = crate::path_key::canonical_recorded_key(&tx, &dest_path_str);

        let existing: Option<(
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        )> = tx
            .query_row(
                "SELECT file_id, media_id, root_id, publication_state, source_path, transfer_mode
                 FROM library_files
                 WHERE library_id = ?1 AND path_key = norm_path_key(?2)",
                params![scope.library_id.to_string(), dest_key],
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
            .optional()?;

        let mut reserved = candidate.clone();

        if let Some((existing_id, existing_media, existing_root, pub_state, src_path, tr_mode)) =
            existing
        {
            if existing_media != candidate.file.media_id.to_string() {
                return Err(StoreError::Invalid(
                    "Transfer destination is already owned by another Media".into(),
                ));
            }
            if pub_state != "pending"
                || existing_root != candidate.file.root_id
                || src_path.as_deref() != Some(source.to_string_lossy().as_ref())
                || tr_mode.as_deref() != Some(mode.as_str())
            {
                return Err(StoreError::Protected(
                    "Transfer destination reservation identity or source changed".into(),
                ));
            }

            let file_id = existing_id.parse::<LedgerId>()?;
            reserved.file.file_id = file_id;
            let units = self.get_file_units(file_id)?;
            if !units.is_empty() {
                reserved.units = units;
            }
        }

        reserved.file.publication = FilePublication::Pending;
        reserved.file.source_path = Some(source.to_path_buf());
        reserved.file.transfer_mode = Some(mode.as_str().to_string());

        // Upsert pending file row
        let file_id_str = reserved.file.file_id.to_string();
        let exists: i64 = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_files WHERE file_id = ?1)",
            params![file_id_str],
            |r| r.get(0),
        )?;

        if exists != 0 {
            ensure_same_reservation(&tx, &file_id_str, scope, &dest_path_str)?;
            tx.execute(
                "UPDATE library_files SET
                    publication_state = 'pending',
                    source_path = ?2,
                    transfer_mode = ?3
                 WHERE file_id = ?1",
                params![
                    file_id_str,
                    source.to_string_lossy().to_string(),
                    mode.as_str()
                ],
            )?;
        } else {
            super::publish::insert_file_row(&tx, &reserved.file)?;
        }

        sync_coverage(&tx, &reserved.file, &reserved.units)?;
        tx.commit()?;

        Ok(reserved)
    }

    /// Mark a transferred file as `Published` with the expected version.
    pub fn finish_transfer(
        &self,
        scope: &WriteScope,
        coverage: &FileCoverage,
        expected: &FileVersion,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(&self.library)?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;

        let file_id_str = coverage.file.file_id.to_string();
        let (library_id, media_id) = persisted_owner(&tx, &file_id_str)?;
        if library_id != scope.library_id.to_string()
            || library_id != coverage.file.library_id.to_string()
            || media_id != coverage.file.media_id.to_string()
        {
            return Err(StoreError::Protected(format!(
                "transfer file {file_id_str} is not owned by write scope {}",
                scope.library_id
            )));
        }
        ensure_media_writable(&tx, coverage.file.media_id)?;
        super::publish::ensure_file_version(&tx, &file_id_str, expected)?;

        let changed = tx.execute(
            "UPDATE library_files SET
                publication_state = 'published'
             WHERE file_id = ?1 AND library_id = ?2 AND file_revision = ?3
               AND source_version = ?4 AND publication_state = 'pending'",
            params![
                file_id_str,
                library_id,
                expected.revision,
                expected.source_version
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Protected(format!(
                "transfer file {file_id_str} is no longer a pending reservation"
            )));
        }

        sync_coverage(&tx, &coverage.file, &coverage.units)?;
        super::publish::enqueue_probe(&tx, coverage.file.file_id, now_ms)?;
        reduce_library_media(&tx, scope.library_id, coverage.file.media_id)?;

        tx.commit()?;
        Ok(())
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
