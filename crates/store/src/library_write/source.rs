//! Source replacement and CAS.
//!
//! When a file source is replaced on disk, the new observation must be committed
//! atomically with an incremented revision and invalidated probe/quality caches.

use domain::{FileVersion, LedgerId};
use library::SourceObservation;
use rusqlite::{OptionalExtension, params};

use super::WriteScope;
use super::publish::{ensure_config_revision, ensure_library_writable, ensure_media_writable};
use super::reduce::reduce_library_media;
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
    /// version was only a placeholder. One transaction: when the row's placement
    /// stat equals the observation and any cold row is of the expected
    /// generation, facts are kept and the cold row is rebound to the new token;
    /// otherwise every derived fact is invalidated. The revision advances and
    /// Probe is re-armed in both cases.
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
        let tx = crate::write_tx(&self.library)?;
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

        if !reset_facts
            && placed_bytes_proven(&tx, &file_id_str, cur_rev, &cur_source, observation)?
        {
            // Same placed bytes: keep hot/cold facts, rebind the cold row to the
            // new token. Context-keyed derived state (fingerprints, probe stages,
            // samples) was keyed by the placeholder token; drop it so Probe
            // regenerates under the real context instead of relabelling it.
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
            tx.execute(
                "UPDATE file_details SET source_revision = ?2, source_version = ?3
                 WHERE file_id = ?1",
                params![file_id_str, new_revision, new_source_version],
            )?;
            crate::library::delete_derived_contexts_for_ledger(&tx, &file_id_str)?;
            super::publish::enqueue_probe(&tx, file_id, now_ms)?;
            tx.commit()?;
            return Ok(FileVersion {
                revision: new_revision,
                source_version: new_source_version,
            });
        }

        advance_to_new_content(&tx, file_id, new_revision, observation, now_ms)?;

        // Re-reduce library media
        reduce_library_media(&tx, scope.library_id, media_id)?;

        tx.commit()?;

        Ok(FileVersion {
            revision: new_revision,
            source_version: new_source_version,
        })
    }
}

/// The file now holds different content: advance to `new_revision` with the new
/// token, reset hot probed facts and drop every derived fact of the old bytes
/// (cold details, fingerprints, probe stages/contexts, samples), then re-arm
/// Probe. Callers reduce the Library card and commit. One statement set, run
/// inside the caller's transaction.
pub(crate) fn advance_to_new_content(
    tx: &rusqlite::Transaction<'_>,
    file_id: LedgerId,
    new_revision: i64,
    observation: &SourceObservation,
    now_ms: i64,
) -> Result<(), StoreError> {
    let file_id_str = file_id.to_string();
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
            observation.source_version,
            observation.size_bytes.map(|s| s as i64),
            observation.modified_ns,
            observation.created_ns,
            observation.content_digest,
            file_id_str,
        ],
    )?;
    crate::library::delete_file_meta_for_ledger(tx, &file_id_str)?;
    super::publish::enqueue_probe(tx, file_id, now_ms)
}

/// First-bind evidence that the observed bytes are the ones placed: the row
/// carries the placement stat (size + mtime) and it equals the observation,
/// and any cold row describes exactly the expected (placeholder) generation.
fn placed_bytes_proven(
    tx: &rusqlite::Transaction<'_>,
    file_id: &str,
    expected_revision: i64,
    expected_source: &str,
    observation: &SourceObservation,
) -> Result<bool, StoreError> {
    let (stat_known, size, mtime): (i64, Option<i64>, Option<i64>) = tx.query_row(
        "SELECT stat_known, size_bytes, mtime_ns FROM library_files WHERE file_id = ?1",
        params![file_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let same_stat = stat_known == 1
        && size.is_some()
        && size == observation.size_bytes.map(|s| s as i64)
        && mtime.is_some()
        && mtime == observation.modified_ns;
    if !same_stat {
        return Ok(false);
    }
    let details: Option<(i64, String)> = tx
        .query_row(
            "SELECT source_revision, source_version FROM file_details WHERE file_id = ?1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(details.is_none_or(|(revision, source)| {
        revision == expected_revision && source == expected_source
    }))
}
