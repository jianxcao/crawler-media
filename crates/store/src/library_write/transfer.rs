//! Transfer reservation and completion transactions.
//!
//! Prepares and publishes FileCoverage facts during Download transfers,
//! maintaining atomic recovery of pending states without a legacy projection shim.

use std::path::Path;

use domain::{FileCoverage, FilePublication, FileVersion, LedgerId};
use rusqlite::{OptionalExtension, params};

use super::publish::{
    ensure_config_revision, ensure_library_writable, ensure_media_writable, sync_coverage,
};
use super::reduce::reduce_library_media;
use super::WriteScope;
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
        let tx = self.library.unchecked_transaction()?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;
        ensure_media_writable(&tx, candidate.file.media_id)?;

        let dest_path_str = candidate.file.path.to_string_lossy().to_string();

        let existing: Option<(String, String, String, String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT file_id, media_id, root_id, publication_state, source_path, transfer_mode
                 FROM library_files
                 WHERE library_id = ?1 AND (path_key = ?2 OR display_path = ?2)",
                params![scope.library_id.to_string(), dest_path_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()?;

        let mut reserved = candidate.clone();

        if let Some((existing_id, existing_media, existing_root, pub_state, src_path, tr_mode)) = existing {
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
            tx.execute(
                "UPDATE library_files SET
                    publication_state = 'pending',
                    source_path = ?2,
                    transfer_mode = ?3
                 WHERE file_id = ?1",
                params![file_id_str, source.to_string_lossy().to_string(), mode.as_str()],
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
        let tx = self.library.unchecked_transaction()?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;

        let file_id_str = coverage.file.file_id.to_string();
        super::publish::ensure_file_version(&tx, &file_id_str, expected)?;

        tx.execute(
            "UPDATE library_files SET
                publication_state = 'published'
             WHERE file_id = ?1",
            params![file_id_str],
        )?;

        sync_coverage(&tx, &coverage.file, &coverage.units)?;
        super::publish::enqueue_probe(&tx, coverage.file.file_id, now_ms)?;
        reduce_library_media(&tx, scope.library_id, coverage.file.media_id)?;

        tx.commit()?;
        Ok(())
    }
}
