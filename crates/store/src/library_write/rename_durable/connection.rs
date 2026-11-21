//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn record_rename_refusal(
        &self,
        rename_id: &str,
        now: i64,
        error: &StoreError,
    ) -> Result<(), StoreError> {
        let terminal = match error {
            StoreError::Protected(msg) => msg != "write admission refused",
            StoreError::Missing(_) => true,
            _ => false,
        };
        if terminal {
            self.mark_file_rename_failed(rename_id, now, &error.to_string())?;
        }
        Ok(())
    }

    pub(super) fn publish_rename_library(
        &self,
        request: &FileRenameRequest,
        effective_sidecars: &[RenameSidecar],
        observed: &library::SourceObservation,
        now: i64,
    ) -> Result<(), StoreError> {
        let rename_id = request.rename_id.as_str();
        let tx = crate::write_tx(self.library)?;
        let id = request.file_id.to_string();
        let from_str = request.from_path.to_string_lossy().into_owned();
        let to_str = request.to_path.to_string_lossy().into_owned();
        let owned = load_owned(&tx, &id, &from_str)?;
        if owned.revision != request.expected.revision
            || owned.source_version != request.expected.source_version
        {
            return Err(StoreError::Protected(format!(
                "file {id} version changed during rename"
            )));
        }
        let from_library = request.from_library_id.to_string();
        let to_library = request.to_library_id.to_string();
        ensure_rename_libraries_admitted(&tx, request, true)?;
        ensure_media_writable(&tx, request.media_id)?;
        refuse_occupied(&tx, &to_str, &id)?;
        let was_unavailable = owned.presence_state != "present" || owned.missing_at.is_some();
        let changed = tx.execute(
            "UPDATE library_files
             SET path_key = norm_path_key(?2), display_path = ?2, root_id = ?3, library_id = ?4,
                 presence_state = 'present', missing_at = NULL
             WHERE file_id = ?1 AND file_revision = ?5 AND source_version = ?6",
            params![
                id,
                to_str,
                request.to_root_id,
                to_library,
                owned.revision,
                owned.source_version,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Protected(format!(
                "file {id} changed during rename publish"
            )));
        }
        let now_ms = now.saturating_mul(1000);
        if observed.source_version != owned.source_version {
            super::super::source::advance_to_new_content(
                &tx,
                request.file_id,
                owned.revision + 1,
                observed,
                now_ms,
            )?;
        } else if was_unavailable {
            crate::ledger_presence::source_restored(&tx, &id, now_ms)?;
        }
        // Coverage rows carry composite FK (file_id, library_id, media_id).
        tx.execute(
            "UPDATE library_file_units SET library_id = ?2 WHERE file_id = ?1",
            params![id, to_library],
        )?;
        // References follow only sidecars whose verified decision placed them.
        let placed: Vec<RenameSidecar> = effective_sidecars
            .iter()
            .filter(|side| side.action.migrates_references())
            .cloned()
            .collect();
        migrate_metadata_inputs(
            &tx,
            &from_library,
            &to_library,
            &request.to_root_id,
            request.media_id,
            &from_str,
            &to_str,
            &placed,
        )?;
        migrate_scrape_sidecars(&tx, &id, &placed)?;
        rewrite_artwork_paths(&tx, &id, &placed)?;
        reduce_library_media(&tx, request.from_library_id, request.media_id)?;
        if request.to_library_id != request.from_library_id {
            reduce_library_media(&tx, request.to_library_id, request.media_id)?;
        }
        if effective_sidecars != request.sidecars.as_slice() {
            tx.execute(
                "UPDATE pending_file_renames SET sidecars_json = ?2 WHERE rename_id = ?1",
                params![rename_id, sidecars_json(effective_sidecars)?],
            )?;
        }
        set_phase(&tx, rename_id, "library_published", now)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn revert_vanished_sidecar_references(
        &self,
        file_id: &str,
        reverted: &[(PathBuf, PathBuf)],
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        for (from, to) in reverted {
            let from_str = from.to_string_lossy().into_owned();
            let to_str = to.to_string_lossy().into_owned();
            let from_key = crate::path_key::canonical_recorded_key(&tx, &from_str);
            let to_key = crate::path_key::canonical_recorded_key(&tx, &to_str);
            tx.execute(
                "UPDATE scrape_sidecars SET path = ?3, path_key = ?4
                 WHERE file_id = ?1 AND path_key = ?2",
                params![file_id, to_key, from_str, from_key],
            )?;
            tx.execute(
                "UPDATE library_files SET
                    poster_path = CASE WHEN poster_path_key = ?2 THEN ?3 ELSE poster_path END,
                    poster_path_key = CASE WHEN poster_path_key = ?2 THEN ?4 ELSE poster_path_key END,
                    backdrop_path = CASE WHEN backdrop_path_key = ?2 THEN ?3 ELSE backdrop_path END,
                    backdrop_path_key = CASE WHEN backdrop_path_key = ?2 THEN ?4 ELSE backdrop_path_key END,
                    still_path = CASE WHEN still_path_key = ?2 THEN ?3 ELSE still_path END,
                    still_path_key = CASE WHEN still_path_key = ?2 THEN ?4 ELSE still_path_key END
                 WHERE file_id = ?1",
                params![file_id, to_key, from_str, from_key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_due_file_rename_ids(&self, now: i64) -> Result<Vec<String>, StoreError> {
        self.list_due_file_rename_ids_limited(now, usize::MAX)
    }

    pub(super) fn file_rename_recovery_schedule(&self) -> Result<Vec<(i64, String)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT next_retry_at, rename_id FROM pending_file_renames
             WHERE phase IN ('planned','disk_applied','library_published','references_applied')
             ORDER BY next_retry_at, created_at_ms, rename_id",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub(super) fn file_rename_retry_at(&self, id: &str) -> Result<Option<i64>, StoreError> {
        Ok(self
            .library
            .query_row(
                "SELECT next_retry_at FROM pending_file_renames WHERE rename_id = ?1
                 AND phase IN ('planned','disk_applied','library_published','references_applied')",
                [id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn list_due_file_rename_ids_limited(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT rename_id FROM pending_file_renames
             WHERE phase IN ('planned','disk_applied','library_published','references_applied')
               AND next_retry_at <= ?1
             ORDER BY next_retry_at, created_at_ms, rename_id LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![now, limit], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn load_file_rename(&self, rename_id: &str) -> Result<FileRenameRequest, StoreError> {
        let raw = self
            .library
            .query_row(
                "SELECT r.file_id, r.from_path, r.to_path, r.expected_revision,
                        r.expected_source_version, r.from_library_id, r.from_root_id,
                        r.to_library_id, r.to_root_id, r.sidecars_json, r.phase,
                        r.source_size, r.source_device, r.source_inode, r.source_mtime_ns,
                        r.from_config_revision, r.to_config_revision, f.media_id,
                        r.dest_size, r.dest_device, r.dest_inode, r.dest_mtime_ns,
                        r.source_digest
                 FROM pending_file_renames r LEFT JOIN library_files f ON f.file_id = r.file_id
                 WHERE r.rename_id = ?1",
                params![rename_id],
                RawRename::read,
            )
            .optional()?
            .ok_or_else(|| StoreError::Missing(format!("rename {rename_id}")))?;
        if raw.file_vanished() {
            return Err(StoreError::Missing(format!(
                "rename {rename_id}: file record vanished"
            )));
        }
        raw.into_request(rename_id)
    }

    pub(super) fn mark_file_rename_failed(
        &self,
        rename_id: &str,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        tracing::error!(rename_id, error, "rename refused; intent marked failed");
        self.library.execute(
            "UPDATE pending_file_renames
             SET last_completed_phase = CASE WHEN phase = 'failed' THEN last_completed_phase ELSE phase END,
                 phase = 'failed', last_error = ?3, updated_at_ms = ?2 * 1000
             WHERE rename_id = ?1 AND phase <> 'failed'",
            params![rename_id, now, error],
        )?;
        Ok(())
    }

    pub(in crate::library_write) fn mark_rename_disk_applied(
        &self,
        rename_id: &str,
        sidecars: &[RenameSidecar],
        now: i64,
    ) -> Result<(), StoreError> {
        let n = self.library.execute(
            "UPDATE pending_file_renames
             SET phase = 'disk_applied', last_completed_phase = 'disk_applied',
                 sidecars_json = ?3, updated_at_ms = ?2 * 1000, last_error = NULL
             WHERE rename_id = ?1 AND phase IN ('planned','disk_applied')",
            params![rename_id, now, sidecars_json(sidecars)?],
        )?;
        if n != 1 {
            return Err(StoreError::Protected(format!(
                "rename {rename_id} missing while marking disk_applied"
            )));
        }
        Ok(())
    }

    pub fn record_file_rename_failure(
        &self,
        rename_id: &str,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE pending_file_renames
             SET attempts = attempts + 1,
                 next_retry_at = ?2 + MIN(86400, 60 * (1 << MIN(attempts, 10))),
                 last_error = ?3,
                 updated_at_ms = ?2 * 1000
             WHERE rename_id = ?1 AND phase <> 'failed'",
            params![rename_id, now, error],
        )?;
        Ok(())
    }
}
