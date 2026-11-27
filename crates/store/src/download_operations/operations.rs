use rusqlite::{Connection, OptionalExtension, params};

use super::types::*;
use super::{parse_opt_id, parse_ts_id_cursor, validate_non_empty};
use crate::plugins::{PluginError, PluginPage, paging};
use crate::{Store, StoreError};

const OP_COLS: &str = "operation_id, idempotency_key, source_kind, subscribe_id, plugin_id, subject, cause_id, site_id, downloader_id, endpoint_fingerprint, enclosure, ownership_tag, info_hash, bytes_digest, verified_bytes, request_digest, state, outcome_code, error_message, revision, created_at, updated_at, submitted_at, reconciled_at";

pub(super) fn map_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<DownloadOperationRecord> {
    let state_raw: String = row.get(16)?;
    let state = state_raw
        .parse()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(DownloadOperationRecord {
        operation_id: row.get(0)?,
        idempotency_key: row.get(1)?,
        source_kind: row.get(2)?,
        subscribe_id: parse_opt_id(row.get(3)?)?,
        plugin_id: row.get(4)?,
        subject: row.get(5)?,
        cause_id: row.get(6)?,
        site_id: parse_opt_id(row.get(7)?)?,
        downloader_id: parse_opt_id(row.get(8)?)?,
        endpoint_fingerprint: row.get(9)?,
        enclosure: row.get(10)?,
        ownership_tag: row.get(11)?,
        info_hash: row.get(12)?,
        bytes_digest: row.get(13)?,
        verified_bytes: row.get(14)?,
        request_digest: row.get(15)?,
        state,
        outcome_code: row.get(17)?,
        error_message: row.get(18)?,
        revision: row.get(19)?,
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
        submitted_at: row.get(22)?,
        reconciled_at: row.get(23)?,
    })
}

pub(super) fn fetch_operation_by_id(
    conn: &Connection,
    operation_id: &str,
) -> Result<Option<DownloadOperationRecord>, StoreError> {
    conn.query_row(
        &format!("SELECT {OP_COLS} FROM download_operations WHERE operation_id = ?1"),
        params![operation_id],
        map_operation,
    )
    .optional()
    .map_err(Into::into)
}

fn fetch_by_idempotency(
    conn: &Connection,
    subject: &str,
    idempotency_key: &str,
) -> Result<Option<DownloadOperationRecord>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {OP_COLS} FROM download_operations WHERE subject = ?1 AND idempotency_key = ?2"
        ),
        params![subject, idempotency_key],
        map_operation,
    )
    .optional()
    .map_err(Into::into)
}

fn validate_accept_input(input: &AcceptDownloadOperationInput) -> Result<(), StoreError> {
    validate_non_empty(&input.operation_id, "operation_id must not be empty")?;
    validate_non_empty(&input.idempotency_key, "idempotency_key must not be empty")?;
    validate_non_empty(&input.source_kind, "source_kind must not be empty")?;
    validate_non_empty(&input.subject, "subject must not be empty")?;
    validate_non_empty(&input.cause_id, "cause_id must not be empty")?;
    validate_non_empty(&input.enclosure, "enclosure must not be empty")?;
    validate_non_empty(&input.ownership_tag, "ownership_tag must not be empty")?;
    validate_non_empty(&input.request_digest, "request_digest must not be empty")?;
    if input.verified_bytes.is_some_and(|v| v < 0) {
        return Err(PluginError::Invalid("verified_bytes must be >= 0").into());
    }
    Ok(())
}

fn insert_intent_operation(
    tx: &Connection,
    input: &AcceptDownloadOperationInput,
) -> Result<DownloadOperationRecord, StoreError> {
    tx.execute(
        "INSERT INTO download_operations (
            operation_id, idempotency_key, source_kind, subscribe_id, plugin_id,
            subject, cause_id, site_id, downloader_id, endpoint_fingerprint,
            enclosure, ownership_tag, info_hash, bytes_digest, verified_bytes,
            request_digest, state, outcome_code, error_message,
            revision, created_at, updated_at, submitted_at, reconciled_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, 'intent', NULL, NULL, 1, ?17, ?17, NULL, NULL
         )",
        params![
            input.operation_id,
            input.idempotency_key,
            input.source_kind,
            input.subscribe_id.map(|id| id.to_string()),
            input.plugin_id,
            input.subject,
            input.cause_id,
            input.site_id.map(|id| id.to_string()),
            input.downloader_id.map(|id| id.to_string()),
            input.endpoint_fingerprint,
            input.enclosure,
            input.ownership_tag,
            input.info_hash,
            input.bytes_digest,
            input.verified_bytes,
            input.request_digest,
            input.now_utc,
        ],
    )?;
    fetch_operation_by_id(tx, &input.operation_id)?.ok_or_else(|| PluginError::NotFound.into())
}

impl Store {
    fn accept_download_operation_inner(
        &self,
        input: &AcceptDownloadOperationInput,
        retry_failed_or_deferred: bool,
    ) -> Result<AcceptedDownloadOperation, StoreError> {
        validate_accept_input(input)?;
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            if let Some(existing) =
                fetch_by_idempotency(&tx, &input.subject, &input.idempotency_key)?
            {
                if existing.request_digest != input.request_digest {
                    return Err(PluginError::IdempotencyConflict.into());
                }
                if retry_failed_or_deferred
                    && matches!(
                        existing.state,
                        DownloadOperationState::Failed | DownloadOperationState::Deferred
                    )
                {
                    let superseded_key = format!(
                        "{}#superseded:{}",
                        existing.idempotency_key, existing.operation_id
                    );
                    tx.execute(
                        "UPDATE download_operations SET idempotency_key = ?2 WHERE operation_id = ?1",
                        params![existing.operation_id, superseded_key],
                    )?;
                } else {
                    tx.commit()?;
                    return Ok(AcceptedDownloadOperation {
                        operation: existing,
                        replayed: true,
                    });
                }
            }
            let created = insert_intent_operation(&tx, input)?;
            tx.commit()?;
            Ok(AcceptedDownloadOperation {
                operation: created,
                replayed: false,
            })
        })
    }

    pub fn accept_download_operation(
        &self,
        input: &AcceptDownloadOperationInput,
    ) -> Result<AcceptedDownloadOperation, StoreError> {
        self.accept_download_operation_inner(input, false)
    }

    pub fn accept_or_retry_download_operation(
        &self,
        input: &AcceptDownloadOperationInput,
    ) -> Result<AcceptedDownloadOperation, StoreError> {
        self.accept_download_operation_inner(input, true)
    }

    pub fn get_download_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<DownloadOperationRecord>, StoreError> {
        validate_non_empty(operation_id, "operation_id must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let row = fetch_operation_by_id(&tx, operation_id)?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn get_download_operation_by_idempotency(
        &self,
        subject: &str,
        idempotency_key: &str,
    ) -> Result<Option<DownloadOperationRecord>, StoreError> {
        validate_non_empty(subject, "subject must not be empty")?;
        validate_non_empty(idempotency_key, "idempotency_key must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let row = fetch_by_idempotency(&tx, subject, idempotency_key)?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn list_download_operations(
        &self,
        filter: &DownloadOperationFilter,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<DownloadOperationRecord>, StoreError> {
        let cap = paging::api_limit(limit);
        let cursor = parse_ts_id_cursor(after)?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {OP_COLS} FROM download_operations
                 WHERE (?1 IS NULL OR subscribe_id = ?1)
                   AND (?2 IS NULL OR plugin_id = ?2)
                   AND (?3 IS NULL OR site_id = ?3)
                   AND (?4 IS NULL OR downloader_id = ?4)
                   AND (?5 IS NULL OR state = ?5)
                   AND (?6 IS NULL OR updated_at < ?6 OR (updated_at = ?6 AND operation_id > ?7))
                 ORDER BY updated_at DESC, operation_id ASC
                 LIMIT ?8"
            ))?;
            let items = stmt
                .query_map(
                    params![
                        filter.subscribe_id.map(|id| id.to_string()),
                        filter.plugin_id,
                        filter.site_id.map(|id| id.to_string()),
                        filter.downloader_id.map(|id| id.to_string()),
                        filter.state.map(DownloadOperationState::as_str),
                        cursor.as_ref().map(|(ts, _)| *ts),
                        cursor.as_ref().map(|(_, id)| id.as_str()),
                        (cap + 1) as i64,
                    ],
                    map_operation,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(items, cap, |r| {
                format!("{}:{}", r.updated_at, r.operation_id)
            }))
        })
    }

    pub fn list_unresolved_download_operations(
        &self,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<DownloadOperationRecord>, StoreError> {
        let cap = paging::api_limit(limit);
        let cursor = parse_ts_id_cursor(after)?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {OP_COLS} FROM download_operations
                 WHERE state IN ('intent','reserved','submitting','unknown')
                   AND (?1 IS NULL OR updated_at > ?1 OR (updated_at = ?1 AND operation_id > ?2))
                 ORDER BY updated_at ASC, operation_id ASC
                 LIMIT ?3"
            ))?;
            let items = stmt
                .query_map(
                    params![
                        cursor.as_ref().map(|(ts, _)| *ts),
                        cursor.as_ref().map(|(_, id)| id.as_str()),
                        (cap + 1) as i64,
                    ],
                    map_operation,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(items, cap, |r| {
                format!("{}:{}", r.updated_at, r.operation_id)
            }))
        })
    }

    pub fn transition_download_operation(
        &self,
        input: &TransitionDownloadOperationInput,
    ) -> Result<DownloadOperationRecord, StoreError> {
        self.transition_download_operation_with_outbox(input, None)
    }

    pub fn transition_download_operation_with_outbox(
        &self,
        input: &TransitionDownloadOperationInput,
        outbox: Option<&crate::plugin_events::EmitOutboxEventInput>,
    ) -> Result<DownloadOperationRecord, StoreError> {
        validate_non_empty(&input.operation_id, "operation_id must not be empty")?;
        if input.expected_revision <= 0 {
            return Err(PluginError::Invalid("expected_revision must be > 0").into());
        }
        if input.verified_bytes.is_some_and(|v| v < 0) {
            return Err(PluginError::Invalid("verified_bytes must be >= 0").into());
        }
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let existing =
                fetch_operation_by_id(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            if existing.state == DownloadOperationState::Unknown {
                return Err(PluginError::UnknownEffect.into());
            }
            let valid_transition = matches!(
                (existing.state, input.next_state),
                (
                    DownloadOperationState::Intent,
                    DownloadOperationState::Reserved
                        | DownloadOperationState::Submitting
                        | DownloadOperationState::Rejected
                        | DownloadOperationState::Deferred
                        | DownloadOperationState::Failed
                ) | (
                    DownloadOperationState::Reserved,
                    DownloadOperationState::Submitting
                        | DownloadOperationState::Rejected
                        | DownloadOperationState::Deferred
                        | DownloadOperationState::Failed
                ) | (
                    DownloadOperationState::Submitting,
                    DownloadOperationState::Submitted
                        | DownloadOperationState::Rejected
                        | DownloadOperationState::Deferred
                        | DownloadOperationState::Failed
                        | DownloadOperationState::Unknown
                )
            );
            if !valid_transition || existing.revision != input.expected_revision {
                return Err(PluginError::Conflict.into());
            }
            let effective_dl = input.downloader_id.or(existing.downloader_id);
            if matches!(
                input.next_state,
                DownloadOperationState::Submitting
                    | DownloadOperationState::Submitted
                    | DownloadOperationState::Unknown
            ) && effective_dl.is_none()
            {
                return Err(PluginError::Invalid("downloader_id required before submit").into());
            }
            tx.execute(
                "UPDATE download_operations
                 SET state = ?3,
                     downloader_id = COALESCE(?4, downloader_id),
                     endpoint_fingerprint = COALESCE(?5, endpoint_fingerprint),
                     info_hash = COALESCE(?6, info_hash),
                     bytes_digest = COALESCE(?7, bytes_digest),
                     verified_bytes = COALESCE(?8, verified_bytes),
                     outcome_code = COALESCE(?9, outcome_code),
                     error_message = COALESCE(?10, error_message),
                     submitted_at = COALESCE(?11, submitted_at),
                     revision = revision + 1,
                     updated_at = ?12
                 WHERE operation_id = ?1 AND revision = ?2",
                params![
                    input.operation_id,
                    input.expected_revision,
                    input.next_state.as_str(),
                    input.downloader_id.map(|id| id.to_string()),
                    input.endpoint_fingerprint,
                    input.info_hash,
                    input.bytes_digest,
                    input.verified_bytes,
                    input.outcome_code,
                    input.error_message,
                    input.submitted_at,
                    input.now_utc,
                ],
            )?;
            if let Some(ev) = outbox {
                crate::plugin_events::emit_outbox_event_in_tx(&tx, ev)?;
            }
            let updated =
                fetch_operation_by_id(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn reconcile_download_operation(
        &self,
        input: &ReconcileDownloadOperationInput,
    ) -> Result<DownloadOperationRecord, StoreError> {
        self.reconcile_download_operation_with_outbox(input, None)
    }

    pub fn reconcile_download_operation_with_outbox(
        &self,
        input: &ReconcileDownloadOperationInput,
        outbox: Option<&crate::plugin_events::EmitOutboxEventInput>,
    ) -> Result<DownloadOperationRecord, StoreError> {
        validate_non_empty(&input.operation_id, "operation_id must not be empty")?;
        if !matches!(
            input.resolved_state,
            DownloadOperationState::Submitted
                | DownloadOperationState::Rejected
                | DownloadOperationState::Failed
        ) {
            return Err(PluginError::Invalid(
                "resolved_state must be submitted, rejected, or failed",
            )
            .into());
        }
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let existing =
                fetch_operation_by_id(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            if !matches!(
                existing.state,
                DownloadOperationState::Submitting | DownloadOperationState::Unknown
            ) {
                return Err(PluginError::Conflict.into());
            }
            if existing.revision != input.expected_revision {
                return Err(PluginError::Conflict.into());
            }
            let submitted_at = if input.resolved_state == DownloadOperationState::Submitted {
                Some(existing.submitted_at.unwrap_or(input.now_utc))
            } else {
                existing.submitted_at
            };
            tx.execute(
                "UPDATE download_operations
                 SET state = ?3,
                     info_hash = COALESCE(?4, info_hash),
                     outcome_code = COALESCE(?5, outcome_code),
                     error_message = COALESCE(?6, error_message),
                     submitted_at = ?7,
                     reconciled_at = ?8,
                     revision = revision + 1,
                     updated_at = ?8
                 WHERE operation_id = ?1 AND revision = ?2",
                params![
                    input.operation_id,
                    input.expected_revision,
                    input.resolved_state.as_str(),
                    input.info_hash,
                    input.outcome_code,
                    input.error_message,
                    submitted_at,
                    input.now_utc,
                ],
            )?;
            if let Some(ownership) = &input.ownership {
                super::ownership::upsert_ownership_in_tx(&tx, ownership)?;
            }
            super::boost::reconcile_reservation_in_tx(
                &tx,
                &input.operation_id,
                input.resolved_state,
                input.boost_task_fact.as_ref(),
                input.now_utc,
            )?;
            if let Some(ev) = outbox {
                crate::plugin_events::emit_outbox_event_in_tx(&tx, ev)?;
            }
            let updated =
                fetch_operation_by_id(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }
}
