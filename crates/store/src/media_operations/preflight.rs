//! Canonical merge preflight (design §4.6).
//!
//! A merge spans `library.db`, `subscribe.db` and the job queue, so every
//! decision that must not be made twice is settled here, before the first write:
//! the canonical operation id, the pinned `metadata_revision` of both sides, the
//! lock conflict against another in-flight operation, and the refresh-target
//! choice for every conflicting alias. Both sides are then frozen
//! (`identity_state = 'merging'`), which is what keeps new references from
//! appearing while the phases drain. A freeze that outlives a crash is intended:
//! resuming the same operation is explicit, every other writer is refused, and
//! the survivor's previous state is restored only when the operation finishes.

use domain::MediaId;
use rusqlite::{Connection, OptionalExtension, params};

use crate::playback::MediaMergeRequest;
use crate::{Store, StoreError};

/// Identity states to restore once the operation finishes. Empty for rows that
/// predate the freeze record.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct FrozenPair {
    #[serde(default)]
    pub survivor_before: String,
    #[serde(default)]
    pub loser_before: String,
}

/// A `(source, kind)` alias pair that both sides claim as their refresh target.
pub(crate) struct RefreshTargetConflict {
    pub source: String,
    pub kind: String,
    pub survivor_external: String,
    pub loser_external: String,
}

/// The operation id is the request id, so a replayed request addresses the same
/// row. Only the canonical hyphenated lowercase form is accepted.
pub(crate) fn canonical_operation_id(request_id: &str) -> Result<String, StoreError> {
    let parsed = uuid::Uuid::parse_str(request_id)
        .map_err(|_| StoreError::Invalid(format!("merge request_id {request_id} is not a UUID")))?;
    if parsed.to_string() != request_id {
        return Err(StoreError::Invalid(format!(
            "merge request_id {request_id} is not a canonical UUID ({parsed})"
        )));
    }
    Ok(parsed.to_string())
}

/// The unfrozen identity states of an operation, read back from its plan.
pub(crate) fn frozen_of_plan(plan_json: &str) -> Result<FrozenPair, StoreError> {
    let value: serde_json::Value = serde_json::from_str(plan_json)?;
    match value.get("frozen") {
        Some(frozen) => Ok(serde_json::from_value(frozen.clone())?),
        None => Ok(FrozenPair::default()),
    }
}

/// The stored plan is the request plus the freeze record. Keeping it flat leaves
/// `json_extract(plan_json, '$.request_id')` and `MediaMergeRequest` decoding of
/// older rows working unchanged.
fn encode_plan(request: &MediaMergeRequest, frozen: &FrozenPair) -> Result<String, StoreError> {
    let mut value = serde_json::to_value(request)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("frozen".to_string(), serde_json::to_value(frozen)?);
    }
    Ok(serde_json::to_string(&value)?)
}

fn same_request(stored: &str, request: &MediaMergeRequest) -> Result<bool, StoreError> {
    let mut stored: serde_json::Value = serde_json::from_str(stored)?;
    if let Some(object) = stored.as_object_mut() {
        object.remove("frozen");
    }
    Ok(stored == serde_json::to_value(request)?)
}

/// The unfinished operation that froze `media_id`, if any.
pub(crate) fn merge_holder(
    conn: &Connection,
    media_id: &str,
) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT operation_id FROM media_operations
             WHERE state <> 'succeeded'
               AND (survivor_id = ?1 OR loser_id = ?1)
             ORDER BY created_at_ms DESC, operation_id DESC
             LIMIT 1",
            params![media_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Refresh targets that both sides claim, so the request must choose one.
pub(crate) fn refresh_target_conflicts(
    conn: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<Vec<RefreshTargetConflict>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT s.source, s.kind, s.external_id, l.external_id
           FROM media_aliases s
           JOIN media_aliases l ON l.source = s.source AND l.kind = s.kind
          WHERE s.media_id = ?1 AND s.is_refresh_target = 1
            AND l.media_id = ?2 AND l.is_refresh_target = 1
          ORDER BY s.source, s.kind",
    )?;
    let rows = stmt
        .query_map(params![survivor.to_string(), loser.to_string()], |r| {
            Ok(RefreshTargetConflict {
                source: r.get(0)?,
                kind: r.get(1)?,
                survivor_external: r.get(2)?,
                loser_external: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// An operation may re-enter a freeze it owns; anyone else is refused.
fn ensure_unlocked(
    conn: &Connection,
    media_id: &str,
    state: &str,
    operation_id: &str,
) -> Result<(), StoreError> {
    if state == "retired" {
        return Err(StoreError::Protected(format!(
            "media {media_id} is retired and cannot be merged"
        )));
    }
    if state != "merging" {
        return Ok(());
    }
    match merge_holder(conn, media_id)? {
        Some(holder) if holder == operation_id => Ok(()),
        Some(holder) => Err(StoreError::Protected(format!(
            "media {media_id} is frozen by merge operation {holder}"
        ))),
        None => Ok(()),
    }
}

fn identity_state(conn: &Connection, media_id: &str) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT identity_state FROM media WHERE media_id = ?1",
            params![media_id],
            |r| r.get(0),
        )
        .optional()?)
}

fn media_kind(conn: &Connection, media_id: &str) -> Result<String, StoreError> {
    Ok(conn.query_row(
        "SELECT kind FROM media WHERE media_id = ?1",
        params![media_id],
        |r| r.get(0),
    )?)
}

fn pin_revision(
    conn: &Connection,
    role: &str,
    media_id: &str,
    expected: i64,
) -> Result<(), StoreError> {
    let current: i64 = conn.query_row(
        "SELECT metadata_revision FROM media WHERE media_id = ?1",
        params![media_id],
        |r| r.get(0),
    )?;
    if current != expected {
        return Err(StoreError::Conflict(format!(
            "merge {role} {media_id} revision mismatch: expected {expected}, got {current}"
        )));
    }
    Ok(())
}

fn require_refresh_targets(
    conn: &Connection,
    request: &MediaMergeRequest,
) -> Result<(), StoreError> {
    for conflict in refresh_target_conflicts(conn, request.survivor, request.loser)? {
        match request.refresh_targets.get(&conflict.source) {
            None => {
                return Err(StoreError::Protected(format!(
                    "merge requires refresh_targets[{}] to choose between {} and {}",
                    conflict.source, conflict.survivor_external, conflict.loser_external
                )));
            }
            Some(chosen)
                if *chosen == conflict.survivor_external || *chosen == conflict.loser_external => {}
            Some(chosen) => {
                return Err(StoreError::Protected(format!(
                    "refresh_targets[{}] = {chosen} is neither {} nor {}",
                    conflict.source, conflict.survivor_external, conflict.loser_external
                )));
            }
        }
    }
    Ok(())
}

impl Store {
    /// Accept or replay a merge request. Nothing is written unless every
    /// preflight decision resolves, so a refused request leaves no trace.
    pub(crate) fn begin_merge(
        &self,
        request: &MediaMergeRequest,
        now_ms: i64,
    ) -> Result<String, StoreError> {
        if request.survivor == request.loser {
            return Err(StoreError::Invalid("cannot merge media with itself".into()));
        }
        let operation_id = canonical_operation_id(&request.request_id)?;
        let tx = self.library.unchecked_transaction()?;

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

/// Overrides of both Media that share a key must not set the same field differently: a
/// user lock is never silently dropped by a merge (R21). Disjoint fields are merged.
fn refuse_override_conflicts(conn: &Connection, survivor: &str, loser: &str) -> Result<(), StoreError> {
    for (survivor_json, loser_json) in shared_override_pairs(conn, survivor, loser)? {
        let s: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&survivor_json)?;
        let l: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&loser_json)?;
        if let Some(field) = l.iter().find_map(|(k, v)| s.get(k).filter(|sv| *sv != v).map(|_| k)) {
            return Err(StoreError::Protected(format!(
                "merge refused: both Media lock field {field} to different values"
            )));
        }
    }
    Ok(())
}

/// `(survivor fields_json, loser fields_json)` for every override key both Media carry.
pub(crate) fn shared_override_pairs(
    conn: &Connection,
    survivor: &str,
    loser: &str,
) -> Result<Vec<(String, String)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT s.fields_json, l.fields_json
           FROM media_metadata_overrides s
           JOIN media_metadata_overrides l
             ON l.entity_scope = s.entity_scope AND l.season = s.season
            AND l.episode = s.episode AND l.locale = s.locale
          WHERE s.media_id = ?1 AND l.media_id = ?2",
    )?;
    let rows = stmt
        .query_map(params![survivor, loser], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
