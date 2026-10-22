//! Source replacement and CAS.
//!
//! When a file source is replaced on disk, the new observation must be committed
//! atomically with an incremented revision and invalidated probe/quality caches.

use domain::{FileVersion, LedgerId};
use library::SourceObservation;
use rusqlite::{OptionalExtension, params};

use super::publish::{ensure_config_revision, ensure_library_writable, ensure_media_writable};
use super::reduce::reduce_library_media;
use super::WriteScope;
use crate::{Store, StoreError};

impl Store {
    /// Atomically replace a file's observed source on disk.
    ///
    /// Validates the library lifecycle and expected FileVersion.
    /// On match, advances revision, invalidates cold details/quality/tracks,
    /// re-enqueues probe obligation, and updates item card in a single transaction.
    pub fn replace_file_source(
        &self,
        scope: &WriteScope,
        file_id: LedgerId,
        expected: &FileVersion,
        observation: &SourceObservation,
        now_ms: i64,
    ) -> Result<FileVersion, StoreError> {
        self.advance_file_source(scope, file_id, expected, observation, now_ms, true)
    }

    /// Bind the first real observation to a freshly placed file whose reservation
    /// version was only a placeholder. Facts already probed from these very bytes
    /// (e.g. by Transfer) are kept; the revision still advances and Probe is
    /// re-armed against the real source version.
    pub fn bind_placed_file_source(
        &self,
        scope: &WriteScope,
        file_id: LedgerId,
        expected: &FileVersion,
        observation: &SourceObservation,
        now_ms: i64,
    ) -> Result<FileVersion, StoreError> {
        self.advance_file_source(scope, file_id, expected, observation, now_ms, false)
    }

    fn advance_file_source(
        &self,
        scope: &WriteScope,
        file_id: LedgerId,
        expected: &FileVersion,
        observation: &SourceObservation,
        now_ms: i64,
        reset_facts: bool,
    ) -> Result<FileVersion, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;

        let file_id_str = file_id.to_string();
        let current: Option<(String, String, i64, String)> = tx
            .query_row(
                "SELECT library_id, media_id, file_revision, source_version
                 FROM library_files WHERE file_id = ?1",
                params![file_id_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        let Some((lib_id, media_id_str, cur_rev, cur_source)) = current else {
            return Err(StoreError::Missing(format!("file {file_id_str}")));
        };

        if lib_id != scope.library_id.to_string() {
            return Err(StoreError::Invalid(
                "file library_id does not match write scope".into(),
            ));
        }

        if cur_rev != expected.revision || cur_source != expected.source_version {
            return Err(StoreError::Protected(format!(
                "StaleVersion for file {file_id_str}: expected r{}v{}, got r{cur_rev}v{cur_source}",
                expected.revision, expected.source_version
            )));
        }

        let media_id = media_id_str.parse::<domain::MediaId>()?;
        ensure_media_writable(&tx, media_id)?;

        let new_revision = cur_rev + 1;
        let new_source_version = observation.source_version.clone();

        if !reset_facts {
            tx.execute(
                "UPDATE library_files SET file_revision = ?1, source_version = ?2,
                    size_bytes = COALESCE(?3, size_bytes), mtime_ns = COALESCE(?4, mtime_ns),
                    created_ns = COALESCE(?5, created_ns), source_digest = ?6, stat_known = 1
                 WHERE file_id = ?7",
                params![
                    new_revision,
                    new_source_version,
                    observation.size_bytes.map(|s| s as i64),
                    observation.modified_ns,
                    observation.created_ns,
                    observation.content_digest,
                    file_id_str,
                ],
            )?;
            super::publish::enqueue_probe(&tx, file_id, now_ms)?;
            tx.commit()?;
            return Ok(FileVersion { revision: new_revision, source_version: new_source_version });
        }

        // Invalidate old probe, cold details, tracks, and hot probed quality
        tx.execute(
            "UPDATE library_files SET
                file_revision = ?1,
                source_version = ?2,
                size_bytes = COALESCE(?3, size_bytes),
                mtime_ns = COALESCE(?4, mtime_ns),
                created_ns = COALESCE(?5, created_ns),
                source_digest = ?6,
                stat_known = 1,
                quality_source = 'release',
                tracks_version = 0,
                duration_ms = NULL,
                codec = NULL,
                hdr = NULL
             WHERE file_id = ?7",
            params![
                new_revision,
                new_source_version,
                observation.size_bytes.map(|s| s as i64),
                observation.modified_ns,
                observation.created_ns,
                observation.content_digest,
                file_id_str,
            ],
        )?;

        // Remove every derived fact of the old bytes: cold details, fingerprints,
        // probe stages and samples.
        crate::library::delete_file_meta_for_ledger(&tx, &file_id_str)?;

        // Re-enqueue probe obligation
        super::publish::enqueue_probe(&tx, file_id, now_ms)?;

        // Re-reduce library media
        reduce_library_media(&tx, scope.library_id, media_id)?;

        tx.commit()?;

        Ok(FileVersion {
            revision: new_revision,
            source_version: new_source_version,
        })
    }
}
