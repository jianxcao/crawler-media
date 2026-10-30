//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn media_operation_status(
        &self,
        operation_id: &str,
    ) -> Result<OperationStatus, StoreError> {
        let row: Option<(String, Option<String>, Option<String>)> = self
            .library
            .query_row(
                "SELECT state, phase_cursor, error FROM media_operations
                 WHERE operation_id = ?1 AND kind = 'merge'",
                params![operation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((state, cursor, error)) = row else {
            return Err(StoreError::Missing(format!(
                "merge operation {operation_id}"
            )));
        };
        Ok(operation_status(
            operation_id,
            valid_merge_phase(operation_id, cursor.as_deref())?,
            &state,
            error,
        ))
    }

    pub fn unfinished_media_merges(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT operation_id FROM media_operations
             WHERE kind = 'merge' AND state <> 'succeeded'
             ORDER BY created_at_ms ASC, operation_id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(super) fn ensure_survivor_accepts_merge(
        &self,
        operation_id: &str,
        survivor: MediaId,
        survivor_str: &str,
    ) -> Result<(), StoreError> {
        let identity: Option<String> = self
            .library
            .query_row(
                "SELECT identity_state FROM media WHERE media_id = ?1",
                params![survivor_str],
                |r| r.get(0),
            )
            .optional()?;
        match identity.as_deref() {
            Some("retired") => Err(StoreError::Protected(format!(
                "merge operation {operation_id}: survivor {survivor} is retired"
            ))),
            Some("merging") => {
                match crate::media_operations::merge_holder(self.library, survivor_str)? {
                    Some(holder) if holder == operation_id => Ok(()),
                    Some(holder) => Err(StoreError::Protected(format!(
                        "merge operation {operation_id}: survivor {survivor} is frozen by merge operation {holder}"
                    ))),
                    None => Ok(()),
                }
            }
            Some(_) => Ok(()),
            None => Err(StoreError::Missing(format!(
                "merge survivor media {survivor}"
            ))),
        }
    }

    pub(super) fn record_merge_failure(
        &self,
        operation_id: &str,
        err: &StoreError,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE media_operations
             SET state = 'failed', error = ?2, updated_at_ms = ?3
             WHERE operation_id = ?1",
            params![operation_id, err.to_string(), now_ms],
        )?;
        Ok(())
    }

    pub(super) fn merge_library_phase(
        &self,
        operation_id: &str,
        survivor: MediaId,
        loser: MediaId,
        request: &MediaMergeRequest,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx, survivor)?;
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx, loser)?;
        let survivor_kind = media_kind(&tx, survivor)?;
        let loser_kind = media_kind(&tx, loser)?;
        if survivor_kind != loser_kind {
            return Err(StoreError::Protected(format!(
                "cannot merge media of different kinds ({survivor_kind} vs {loser_kind})"
            )));
        }

        move_aliases(&tx, survivor, loser, &request.refresh_targets)?;
        let libraries = affected_libraries(&tx, survivor, loser)?;
        move_files(&tx, survivor, loser)?;
        move_nfo_inputs(&tx, survivor, loser)?;
        merge_metadata(&tx, survivor, loser)?;
        merge_playback_units(&tx, survivor, loser)?;
        merge_collection_items(&tx, survivor, loser)?;
        merge_watch_facts(&tx, survivor, loser)?;
        for library in &libraries {
            let library_id: LibraryId = library.parse()?;
            reduce_library_media(&tx, library_id, survivor)?;
        }
        // The summaries are derived from the user's playback facts, so they are
        // only correct after the loser's rows moved onto the survivor.
        self.refresh_merged_summaries(&tx, survivor)?;
        tx.execute(
            "UPDATE media_operations
             SET state = 'applying', phase_cursor = ?2, error = NULL, updated_at_ms = ?3
             WHERE operation_id = ?1",
            params![operation_id, PHASE_SUBSCRIBES, now_ms],
        )?;
        tx.commit()?;
        tracing::info!(
            operation_id,
            %survivor,
            %loser,
            libraries = libraries.len(),
            "Media merge: library facts applied"
        );
        Ok(())
    }

    pub(super) fn merge_retire_phase(
        &self,
        operation_id: &str,
        survivor: MediaId,
        loser: MediaId,
        frozen: &crate::media_operations::FrozenPair,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        ensure_loser_has_no_references(&tx, loser)?;
        tx.execute(
            "UPDATE media SET identity_state = 'retired', survivor_id = ?1
             WHERE media_id = ?2",
            params![survivor.to_string(), loser.to_string()],
        )?;
        tx.execute(
            "UPDATE media SET identity_state = ?2
             WHERE media_id = ?1 AND identity_state = 'merging'",
            params![survivor.to_string(), frozen.survivor_before],
        )?;
        // The library phase reduced cards while the survivor was frozen. Identity
        // does not change search content or memberships, but must be published
        // alongside the final raw identity in this transaction.
        tx.execute(
            "UPDATE library_items SET identity_state = ?2,
                 item_revision = item_revision + 1,
                 query_projection_revision = CASE
                     WHEN query_projection_revision = item_revision THEN item_revision + 1
                     ELSE query_projection_revision END
             WHERE media_id = ?1 AND identity_state = 'merging'",
            params![survivor.to_string(), frozen.survivor_before],
        )?;
        tx.execute(
            "UPDATE media_operations
             SET state = 'succeeded', phase_cursor = ?2, error = NULL, updated_at_ms = ?3
             WHERE operation_id = ?1",
            params![operation_id, PHASE_COMPLETE, now_ms],
        )?;
        tx.commit()?;
        tracing::info!(operation_id, %survivor, %loser, "Media merge: loser retired");
        Ok(())
    }
}
