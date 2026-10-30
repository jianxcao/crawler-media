//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
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

    pub(super) fn advance_file_source(
        &self,
        scope: &WriteScope,
        file_id: LedgerId,
        expected: &FileVersion,
        observation: &SourceObservation,
        now_ms: i64,
        reset_facts: bool,
    ) -> Result<FileVersion, StoreError> {
        let tx = crate::write_tx(self.library)?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;

        let file_id_str = file_id.to_string();
        let current: Option<(String, String, i64, String, String)> = tx
            .query_row(
                "SELECT library_id, media_id, file_revision, source_version, path_key
                 FROM library_files WHERE file_id = ?1",
                params![file_id_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;

        let Some((lib_id, media_id_str, cur_rev, cur_source, path_key)) = current else {
            return Err(StoreError::Missing(format!("file {file_id_str}")));
        };
        crate::pending_file_deletions::ensure_path_not_pending_deletion(&tx, &path_key)?;

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
            super::super::publish::enqueue_probe(&tx, file_id, now_ms)?;
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
