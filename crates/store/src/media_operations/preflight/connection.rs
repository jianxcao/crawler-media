//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub(crate) fn begin_merge(
        &self,
        request: &MediaMergeRequest,
        now_ms: i64,
    ) -> Result<String, StoreError> {
        if request.survivor == request.loser {
            return Err(StoreError::Invalid("cannot merge media with itself".into()));
        }
        let operation_id = canonical_operation_id(&request.request_id)?;
        let tx = crate::write_tx(self.library)?;

        crate::library_operations::integrity_guard::ensure_media_integrity_writable(
            &tx,
            request.survivor,
        )?;
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(
            &tx,
            request.loser,
        )?;
        let stored: Option<String> = tx
            .query_row(
                "SELECT plan_json FROM media_operations WHERE operation_id = ?1",
                params![operation_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(stored) = stored {
            if same_request(&stored, request)? {
                return Ok(operation_id);
            }
            return Err(StoreError::Conflict(format!(
                "merge operation {operation_id} already exists with a different plan"
            )));
        }

        let survivor_id = request.survivor.to_string();
        let loser_id = request.loser.to_string();
        let survivor_before = identity_state(&tx, &survivor_id)?.ok_or_else(|| {
            StoreError::Missing(format!("merge survivor media {}", request.survivor))
        })?;
        let loser_before = identity_state(&tx, &loser_id)?
            .ok_or_else(|| StoreError::Missing(format!("merge loser media {}", request.loser)))?;
        ensure_unlocked(&tx, &survivor_id, &survivor_before, &operation_id)?;
        ensure_unlocked(&tx, &loser_id, &loser_before, &operation_id)?;

        let survivor_kind = media_kind(&tx, &survivor_id)?;
        let loser_kind = media_kind(&tx, &loser_id)?;
        if survivor_kind != loser_kind {
            return Err(StoreError::Protected(format!(
                "cannot merge media of different kinds ({survivor_kind} vs {loser_kind})"
            )));
        }
        pin_revision(&tx, "survivor", &survivor_id, request.survivor_revision)?;
        pin_revision(&tx, "loser", &loser_id, request.loser_revision)?;
        require_refresh_targets(&tx, request)?;
        validate_target_entity_snapshots(&tx, request)?;
        refuse_override_conflicts(&tx, &survivor_id, &loser_id)?;

        let frozen = FrozenPair {
            survivor_before,
            loser_before,
        };
        for media_id in [&survivor_id, &loser_id] {
            tx.execute(
                "UPDATE media SET identity_state = 'merging'
                 WHERE media_id = ?1 AND identity_state <> 'merging'",
                params![media_id],
            )?;
        }
        let plan_json = encode_plan(request, &frozen)?;
        tx.execute(
            "INSERT INTO media_operations
                (operation_id, kind, state, survivor_id, loser_id, plan_json, phase_cursor, error, created_at_ms, updated_at_ms)
             VALUES
                (?1, 'merge', 'frozen', ?2, ?3, ?4, 'init', NULL, ?5, ?5)",
            params![operation_id, survivor_id, loser_id, plan_json, now_ms],
        )?;
        tx.commit()?;
        tracing::info!(
            operation_id,
            survivor = %request.survivor,
            loser = %request.loser,
            "Media merge: both identities frozen, request accepted"
        );
        Ok(operation_id)
    }
}
