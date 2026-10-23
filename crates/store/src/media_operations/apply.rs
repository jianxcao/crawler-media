//! Canonical Media identity merge apply phases (design §4.6).
//!
//! The merge runs in persisted phases recorded on the `media_operations` row:
//!
//! 1. `init` → `subscribes`: one `library.db` transaction moves the loser's
//!    files/coverage, aliases, adopted metadata, user playback facts and
//!    collection membership, then rebuilds the survivor's summaries.
//! 2. `subscribes` → `retire`: `subscribes` lives in `subscribe.db`, so this is
//!    an explicit, idempotent, phase-guarded cross-file step. There is no
//!    cross-file transaction and this module never claims one.
//! 3. `retire`: verifies the loser has no remaining references, writes the
//!    retired tombstone and closes the operation.
//!
//! Every phase is safe to re-run after a crash: files/aliases/playback rows are
//! moved or deleted, metadata merges are deterministic from the surviving rows,
//! and the subscribe update is a plain `UPDATE`.

use std::collections::BTreeMap;

use domain::{LibraryId, MediaId};
use rusqlite::{Connection, OptionalExtension, params};

use super::references::{
    merge_collection_items, merge_metadata, merge_playback_units, merge_watch_facts,
};
use crate::library_write::reduce::reduce_library_media;
use crate::playback::types::{MediaMergeRequest, OperationStatus};
use crate::{Store, StoreError};

const PHASE_INIT: &str = "init";
const PHASE_SUBSCRIBES: &str = "subscribes";
const PHASE_RETIRE: &str = "retire";
const PHASE_COMPLETE: &str = "complete";

impl Store {
    /// Read-only status of one merge operation, so an HTTP caller can poll a
    /// merge it started without driving it forward.
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
            cursor.as_deref().unwrap_or(PHASE_INIT),
            &state,
            error,
        ))
    }

    /// Merge operations that still need driving, oldest first. The runtime
    /// resumes these after a restart; frozen identities stay resumable.
    pub fn unfinished_media_merges(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT operation_id FROM media_operations
             WHERE kind = 'merge' AND state <> 'succeeded'
             ORDER BY created_at_ms ASC, operation_id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Drive one merge operation forward through its persisted phases.
    pub(crate) fn advance_merge_operation(
        &self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        let row: Option<(String, String, Option<String>, String, Option<String>)> = self
            .library
            .query_row(
                "SELECT state, survivor_id, loser_id, plan_json, phase_cursor
                 FROM media_operations WHERE operation_id = ?1 AND kind = 'merge'",
                params![operation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((state, survivor_str, loser_str, plan_json, phase_cursor)) = row else {
            return Err(StoreError::Missing(format!(
                "merge operation {operation_id}"
            )));
        };

        let survivor: MediaId = survivor_str.parse()?;
        self.ensure_survivor_accepts_merge(operation_id, survivor, &survivor_str)?;

        if state == "succeeded" {
            return Ok(operation_status(
                operation_id,
                PHASE_COMPLETE,
                "succeeded",
                None,
            ));
        }

        let loser_str = loser_str.ok_or_else(|| {
            StoreError::Invalid(format!("merge operation {operation_id} has no loser"))
        })?;
        let loser: MediaId = loser_str.parse()?;
        if survivor == loser {
            return Err(StoreError::Invalid("cannot merge media with itself".into()));
        }
        let request: MediaMergeRequest = serde_json::from_str(&plan_json)?;
        let frozen = crate::media_operations::frozen_of_plan(&plan_json)?;
        let mut phase = phase_cursor.unwrap_or_else(|| PHASE_INIT.to_string());

        // Each phase persists its successor before returning, so a crash at any
        // point resumes at the same boundary. Drive them to completion here.
        let result = loop {
            let step = match phase.as_str() {
                PHASE_INIT | "planned" | "frozen" | "draining" | "applying" => {
                    self.merge_library_phase(operation_id, survivor, loser, &request, now_ms)
                }
                PHASE_SUBSCRIBES => {
                    self.merge_subscribe_phase(operation_id, survivor, loser, now_ms)
                }
                PHASE_RETIRE => {
                    break self.merge_retire_phase(operation_id, survivor, loser, &frozen, now_ms);
                }
                PHASE_COMPLETE => break Ok(()),
                other => {
                    break Err(StoreError::Invalid(format!(
                        "merge operation {operation_id} has unknown phase {other}"
                    )));
                }
            };
            if let Err(err) = step {
                break Err(err);
            }
            phase = self
                .library
                .query_row(
                    "SELECT phase_cursor FROM media_operations WHERE operation_id = ?1",
                    params![operation_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .unwrap_or_else(|| PHASE_COMPLETE.to_string());
        };

        match result {
            Ok(()) => Ok(operation_status(
                operation_id,
                PHASE_COMPLETE,
                "succeeded",
                None,
            )),
            Err(err) => {
                tracing::error!(operation_id, error = %err, "Media merge phase failed");
                if let Err(record) = self.record_merge_failure(operation_id, &err, now_ms) {
                    tracing::error!(
                        operation_id,
                        error = %record,
                        "Failed to record merge failure"
                    );
                }
                Err(err)
            }
        }
    }

    /// A retired survivor can never take work, and a freeze belongs to exactly
    /// one operation: only the operation named by the freeze may continue, any
    /// other merge is refused. A failed operation keeps its freeze and is retried
    /// (R21), so it is never taken over by a newer one.
    fn ensure_survivor_accepts_merge(
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
                match crate::media_operations::merge_holder(&self.library, survivor_str)? {
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

    fn record_merge_failure(
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

    /// Phase 1: every `library.db` fact move plus the survivor's summary rebuild,
    /// in one transaction. The phase cursor is advanced inside that transaction.
    fn merge_library_phase(
        &self,
        operation_id: &str,
        survivor: MediaId,
        loser: MediaId,
        request: &MediaMergeRequest,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(&self.library)?;
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

    /// Phase 2: re-point `subscribes.media_id` in `subscribe.db`. The phase was
    /// persisted by phase 1 before this step ran; a crash here re-runs the same
    /// idempotent `UPDATE`.
    fn merge_subscribe_phase(
        &self,
        operation_id: &str,
        survivor: MediaId,
        loser: MediaId,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(&self.subscribe)?;
        let moved = tx.execute(
            "UPDATE subscribes SET media_id = ?1 WHERE media_id = ?2",
            params![survivor.to_string(), loser.to_string()],
        )?;
        tx.commit()?;
        tracing::info!(
            operation_id,
            %survivor,
            %loser,
            moved,
            "Media merge: subscribe references re-pointed"
        );

        self.library.execute(
            "UPDATE media_operations
             SET phase_cursor = ?2, state = 'applying', error = NULL, updated_at_ms = ?3
             WHERE operation_id = ?1",
            params![operation_id, PHASE_RETIRE, now_ms],
        )?;
        Ok(())
    }

    /// Phase 3: close the operation with the loser's tombstone. The loser must
    /// already have no rows left in `library.db`. The survivor is unfrozen only
    /// here, restoring the identity state it had before the preflight froze it.
    fn merge_retire_phase(
        &self,
        operation_id: &str,
        survivor: MediaId,
        loser: MediaId,
        frozen: &crate::media_operations::FrozenPair,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(&self.library)?;
        ensure_loser_has_no_references(&tx, loser)?;
        tx.execute(
            "UPDATE media SET identity_state = 'retired', survivor_id = ?1
             WHERE media_id = ?2",
            params![survivor.to_string(), loser.to_string()],
        )?;
        if !frozen.survivor_before.is_empty() {
            tx.execute(
                "UPDATE media SET identity_state = ?2
                 WHERE media_id = ?1 AND identity_state = 'merging'",
                params![survivor.to_string(), frozen.survivor_before],
            )?;
        }
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

fn operation_status(
    operation_id: &str,
    phase: &str,
    state: &str,
    error: Option<String>,
) -> OperationStatus {
    OperationStatus {
        operation_id: operation_id.to_string(),
        phase: phase.to_string(),
        state: state.to_string(),
        error,
    }
}

fn media_kind(tx: &Connection, media: MediaId) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT kind FROM media WHERE media_id = ?1",
        params![media.to_string()],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| StoreError::Missing(format!("media {media}")))
}

fn affected_libraries(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<Vec<String>, StoreError> {
    let mut stmt =
        tx.prepare("SELECT DISTINCT library_id FROM library_files WHERE media_id IN (?1, ?2)")?;
    let rows = stmt
        .query_map(params![survivor.to_string(), loser.to_string()], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Adopt the loser's NFO/export inputs. The survivor's own observation of a
/// sidecar is authoritative: when both sides hold the same key the loser's row is
/// dropped rather than overwriting the survivor's export. Runs before the summary
/// reduction so the adopted inputs are pruned against the merged file set.
fn move_nfo_inputs(tx: &Connection, survivor: MediaId, loser: MediaId) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_metadata_inputs
          WHERE media_id = ?1
            AND EXISTS (
                SELECT 1 FROM library_metadata_inputs AS keep
                 WHERE keep.media_id = ?2
                   AND keep.library_id = library_metadata_inputs.library_id
                   AND keep.entity_scope = library_metadata_inputs.entity_scope
                   AND keep.season = library_metadata_inputs.season
                   AND keep.episode = library_metadata_inputs.episode
                   AND keep.locale = library_metadata_inputs.locale
                   AND keep.input_key = library_metadata_inputs.input_key
            )",
        params![loser.to_string(), survivor.to_string()],
    )?;
    tx.execute(
        "UPDATE library_metadata_inputs SET media_id = ?1 WHERE media_id = ?2",
        params![survivor.to_string(), loser.to_string()],
    )?;
    Ok(())
}

/// Move the loser's files and coverage, dropping its derived summaries first so
/// no `preferred_file_id` points at a row whose owner just changed.
fn move_files(tx: &Connection, survivor: MediaId, loser: MediaId) -> Result<(), StoreError> {
    let (survivor, loser) = (survivor.to_string(), loser.to_string());
    tx.execute(
        "DELETE FROM library_units WHERE media_id = ?1",
        params![loser],
    )?;
    tx.execute(
        "DELETE FROM library_items WHERE media_id = ?1",
        params![loser],
    )?;
    tx.execute(
        "UPDATE library_files SET media_id = ?1 WHERE media_id = ?2",
        params![survivor, loser],
    )?;
    tx.execute(
        "UPDATE library_file_units SET media_id = ?1 WHERE media_id = ?2",
        params![survivor, loser],
    )?;
    Ok(())
}

/// Move the loser's aliases onto the survivor while keeping exactly one refresh
/// target per `(media, source, kind)`. A conflict between two refresh targets is
/// resolved only by the request, which the preflight has already validated;
/// re-deciding it here keeps the phase safe to re-run after a crash.
fn move_aliases(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
    refresh_targets: &BTreeMap<String, String>,
) -> Result<(), StoreError> {
    for conflict in crate::media_operations::refresh_target_conflicts(tx, survivor, loser)? {
        match refresh_targets.get(&conflict.source) {
            Some(chosen) if *chosen == conflict.survivor_external => {
                tx.execute(
                    "UPDATE media_aliases SET is_refresh_target = 0
                     WHERE source = ?1 AND kind = ?2 AND external_id = ?3",
                    params![conflict.source, conflict.kind, conflict.loser_external],
                )?;
            }
            Some(chosen) if *chosen == conflict.loser_external => {
                tx.execute(
                    "UPDATE media_aliases SET is_refresh_target = 0
                     WHERE media_id = ?1 AND source = ?2 AND kind = ?3
                       AND is_refresh_target = 1",
                    params![survivor.to_string(), conflict.source, conflict.kind],
                )?;
            }
            Some(chosen) => {
                return Err(StoreError::Protected(format!(
                    "refresh_targets[{}] = {chosen} is neither {} nor {}",
                    conflict.source, conflict.survivor_external, conflict.loser_external
                )));
            }
            None => {
                return Err(StoreError::Protected(format!(
                    "merge requires refresh_targets[{}] to choose between {} and {}",
                    conflict.source, conflict.survivor_external, conflict.loser_external
                )));
            }
        }
    }

    let mut stmt = tx.prepare(
        "SELECT source, kind, external_id, is_refresh_target
         FROM media_aliases WHERE media_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![loser.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    for (source, kind, external_id, _is_target) in rows {
        tx.execute(
            "UPDATE media_aliases SET media_id = ?1
             WHERE source = ?2 AND kind = ?3 AND external_id = ?4",
            params![survivor.to_string(), source, kind, external_id],
        )?;
    }
    Ok(())
}

/// The retired tombstone is only written once every mergeable reference has
/// moved. A leftover row means a phase did not finish; refuse to retire.
fn ensure_loser_has_no_references(tx: &Connection, loser: MediaId) -> Result<(), StoreError> {
    let loser = loser.to_string();
    let tables = [
        "library_files",
        "library_file_units",
        "library_units",
        "library_items",
        "library_metadata_inputs",
        "media_aliases",
        "media_metadata",
        "media_metadata_sources",
        "media_metadata_overrides",
        "media_seasons",
        "media_episodes",
        "playback_units",
        "collection_items",
        "media_markers",
        "fingerprint_season_models",
        "probe_season_comparison_history",
        "library_item_terms",
        "user_library_items",
    ];
    let mut leftover = Vec::new();
    for table in tables {
        let count: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE media_id = ?1"),
            params![loser],
            |r| r.get(0),
        )?;
        if count > 0 {
            leftover.push(format!("{table}={count}"));
        }
    }
    if leftover.is_empty() {
        Ok(())
    } else {
        Err(StoreError::Invalid(format!(
            "cannot retire media {loser}: references remain ({})",
            leftover.join(", ")
        )))
    }
}
