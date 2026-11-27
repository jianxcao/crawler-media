use rusqlite::{params, params_from_iter, types::Value};

use super::sanitize::sanitize_delivery_error;
use super::types::{
    ClaimDueDeliveriesInput, ClaimedOutboxDelivery, MarkDeadLetterInput, MarkDeliveredInput,
    OutboxDb, OutboxDeliveryRecord, OutboxEventRecord, ScheduleRetryInput,
};
use super::{
    OUTBOX_COLUMNS, fetch_delivery, map_outbox_event, prefixed_columns, validate_non_empty,
    validate_opt_non_empty,
};
use crate::plugins::{PluginError, paging};
use crate::{Store, StoreError};

fn strip_permanent_failed_component_state(raw: &str) -> String {
    let Ok(mut root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return "{}".into();
    };
    if let Some(done) = root.get_mut("done").and_then(|v| v.as_object_mut()) {
        done.retain(|_, status| status.as_str() == Some("succeeded"));
    }
    serde_json::to_string(&root).unwrap_or_else(|_| "{}".into())
}

fn ensure_delivery_conflict_or_not_found(
    conn: &rusqlite::Connection,
    event_id: &str,
    plugin_id: &str,
) -> StoreError {
    match fetch_delivery(conn, event_id, plugin_id) {
        Ok(Some(_)) => PluginError::Conflict.into(),
        Ok(None) => PluginError::NotFound.into(),
        Err(err) => err,
    }
}

impl Store {
    /// Atomically claims due deliveries (or expired leases) that are at the head
    /// of line for their `(plugin_id, entity_type, entity_id)`, incrementing
    /// `attempt_count` by 1 and setting `claim_nonce` + `lease_until`.
    pub fn claim_due_deliveries(
        &self,
        db: OutboxDb,
        input: &ClaimDueDeliveriesInput,
    ) -> Result<Vec<ClaimedOutboxDelivery>, StoreError> {
        validate_opt_non_empty(input.plugin_id.as_deref(), "plugin_id must not be empty")?;
        if input.now_ms < 0 {
            return Err(PluginError::Invalid("now_ms must be >= 0").into());
        }
        if input.lease_duration_ms <= 0 {
            return Err(PluginError::Invalid("lease_duration_ms must be > 0").into());
        }
        let lease_until = input
            .now_ms
            .checked_add(input.lease_duration_ms)
            .ok_or(PluginError::Invalid("lease_until overflow"))?;
        let cap = paging::api_limit(input.limit);

        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let o_cols = prefixed_columns("o", OUTBOX_COLUMNS);
            let mut conds = vec![
                "((d.status = 'pending' AND d.next_attempt_at <= ?1) \
                  OR (d.status = 'delivering' AND d.lease_until <= ?1))"
                    .to_string(),
                "NOT EXISTS ( \
                    SELECT 1 FROM plugin_event_deliveries active \
                    WHERE active.plugin_id = d.plugin_id \
                      AND active.entity_type = d.entity_type \
                      AND active.entity_id = d.entity_id \
                      AND active.status IN ('pending', 'delivering') \
                      AND active.status = 'delivering' \
                      AND active.entity_sequence != d.entity_sequence \
                 )"
                .to_string(),
                "(d.status = 'delivering' OR NOT EXISTS ( \
                    SELECT 1 FROM plugin_event_deliveries prev \
                    WHERE prev.plugin_id = d.plugin_id \
                      AND prev.entity_type = d.entity_type \
                      AND prev.entity_id = d.entity_id \
                      AND prev.entity_sequence < d.entity_sequence \
                      AND prev.status IN ('pending', 'delivering') \
                 ))"
                .to_string(),
            ];
            let mut bind: Vec<Value> = vec![Value::Integer(input.now_ms)];
            if let Some(pid) = &input.plugin_id {
                bind.push(Value::Text(pid.clone()));
                conds.push(format!("d.plugin_id = ?{}", bind.len()));
            }
            bind.push(Value::Integer(cap as i64));
            let select_sql = format!(
                "SELECT {o_cols}, d.plugin_id, d.revision
                 FROM plugin_event_deliveries d
                 JOIN plugin_event_outbox o ON o.event_id = d.event_id
                 WHERE {}
                 ORDER BY d.next_attempt_at ASC, d.event_id ASC, d.plugin_id ASC
                 LIMIT ?{}",
                conds.join(" AND "),
                bind.len(),
            );
            let mut stmt = tx.prepare(&select_sql)?;
            let candidates: Vec<(OutboxEventRecord, String, i64)> = stmt
                .query_map(params_from_iter(bind), |row| {
                    Ok((map_outbox_event(row, 0)?, row.get(18)?, row.get(19)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);

            let mut claimed = Vec::with_capacity(candidates.len());
            for (event, plugin_id, expected_rev) in candidates {
                let claim_nonce = uuid::Uuid::new_v4().to_string();
                let updated = tx.execute(
                    "UPDATE plugin_event_deliveries
                     SET status = 'delivering',
                         attempt_count = attempt_count + 1,
                         claimed_at = ?4,
                         lease_until = ?5,
                         claim_nonce = ?6,
                         revision = revision + 1,
                         updated_at = ?4
                     WHERE event_id = ?1 AND plugin_id = ?2 AND revision = ?3",
                    params![
                        event.event_id,
                        plugin_id,
                        expected_rev,
                        input.now_ms,
                        lease_until,
                        claim_nonce,
                    ],
                )?;
                if updated == 0 {
                    continue;
                }
                let delivery = fetch_delivery(&tx, &event.event_id, &plugin_id)?
                    .ok_or(PluginError::NotFound)?;
                claimed.push(ClaimedOutboxDelivery {
                    event,
                    delivery,
                    claim_nonce,
                });
            }
            tx.commit()?;
            Ok(claimed)
        })
    }

    pub fn mark_delivered(
        &self,
        db: OutboxDb,
        input: &MarkDeliveredInput,
    ) -> Result<OutboxDeliveryRecord, StoreError> {
        validate_non_empty(&input.event_id, "event_id must not be empty")?;
        validate_non_empty(&input.plugin_id, "plugin_id must not be empty")?;
        validate_non_empty(&input.claim_nonce, "claim_nonce must not be empty")?;
        if input.now_ms < 0 {
            return Err(PluginError::Invalid("now_ms must be >= 0").into());
        }
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let changed = tx.execute(
                "UPDATE plugin_event_deliveries
                 SET status = 'delivered',
                     claimed_at = NULL,
                     lease_until = NULL,
                     claim_nonce = NULL,
                     last_error = NULL,
                     delivered_at = ?4,
                     revision = revision + 1,
                     updated_at = ?4
                 WHERE event_id = ?1 AND plugin_id = ?2
                   AND status = 'delivering' AND claim_nonce = ?3",
                params![
                    input.event_id,
                    input.plugin_id,
                    input.claim_nonce,
                    input.now_ms,
                ],
            )?;
            if changed == 0 {
                return Err(ensure_delivery_conflict_or_not_found(
                    &tx,
                    &input.event_id,
                    &input.plugin_id,
                ));
            }
            let updated = fetch_delivery(&tx, &input.event_id, &input.plugin_id)?
                .ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn schedule_retry(
        &self,
        db: OutboxDb,
        input: &ScheduleRetryInput,
    ) -> Result<OutboxDeliveryRecord, StoreError> {
        validate_non_empty(&input.event_id, "event_id must not be empty")?;
        validate_non_empty(&input.plugin_id, "plugin_id must not be empty")?;
        validate_non_empty(&input.claim_nonce, "claim_nonce must not be empty")?;
        if input.now_ms < 0 || input.next_attempt_at < input.now_ms {
            return Err(PluginError::Invalid("next_attempt_at must be >= now_ms >= 0").into());
        }
        let clean_error = sanitize_delivery_error(&input.error);
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let changed = tx.execute(
                "UPDATE plugin_event_deliveries
                 SET status = 'pending',
                     next_attempt_at = ?4,
                     claimed_at = NULL,
                     lease_until = NULL,
                     claim_nonce = NULL,
                     last_error = ?5,
                     revision = revision + 1,
                     updated_at = ?6
                 WHERE event_id = ?1 AND plugin_id = ?2
                   AND status = 'delivering' AND claim_nonce = ?3",
                params![
                    input.event_id,
                    input.plugin_id,
                    input.claim_nonce,
                    input.next_attempt_at,
                    clean_error,
                    input.now_ms,
                ],
            )?;
            if changed == 0 {
                return Err(ensure_delivery_conflict_or_not_found(
                    &tx,
                    &input.event_id,
                    &input.plugin_id,
                ));
            }
            let updated = fetch_delivery(&tx, &input.event_id, &input.plugin_id)?
                .ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn mark_dead_letter(
        &self,
        db: OutboxDb,
        input: &MarkDeadLetterInput,
    ) -> Result<OutboxDeliveryRecord, StoreError> {
        validate_non_empty(&input.event_id, "event_id must not be empty")?;
        validate_non_empty(&input.plugin_id, "plugin_id must not be empty")?;
        validate_non_empty(&input.claim_nonce, "claim_nonce must not be empty")?;
        if input.now_ms < 0 {
            return Err(PluginError::Invalid("now_ms must be >= 0").into());
        }
        let clean_error = sanitize_delivery_error(&input.error);
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let changed = tx.execute(
                "UPDATE plugin_event_deliveries
                 SET status = 'dead_letter',
                     claimed_at = NULL,
                     lease_until = NULL,
                     claim_nonce = NULL,
                     last_error = ?4,
                     dead_lettered_at = ?5,
                     revision = revision + 1,
                     updated_at = ?5
                 WHERE event_id = ?1 AND plugin_id = ?2
                   AND status = 'delivering' AND claim_nonce = ?3",
                params![
                    input.event_id,
                    input.plugin_id,
                    input.claim_nonce,
                    clean_error,
                    input.now_ms,
                ],
            )?;
            if changed == 0 {
                return Err(ensure_delivery_conflict_or_not_found(
                    &tx,
                    &input.event_id,
                    &input.plugin_id,
                ));
            }
            let updated = fetch_delivery(&tx, &input.event_id, &input.plugin_id)?
                .ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn requeue_dead_letter(
        &self,
        db: OutboxDb,
        event_id: &str,
        plugin_id: &str,
        next_attempt_at: i64,
        now_ms: i64,
    ) -> Result<OutboxDeliveryRecord, StoreError> {
        validate_non_empty(event_id, "event_id must not be empty")?;
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        if now_ms < 0 || next_attempt_at < 0 {
            return Err(PluginError::Invalid("timestamps must be >= 0").into());
        }
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let existing = fetch_delivery(&tx, event_id, plugin_id)?;
            let Some(existing) = existing else {
                return Err(PluginError::NotFound.into());
            };
            if existing.status != super::types::OutboxDeliveryStatus::DeadLetter {
                return Err(PluginError::Conflict.into());
            }
            let cleaned_state =
                strip_permanent_failed_component_state(&existing.component_state_json);
            let changed = tx.execute(
                "UPDATE plugin_event_deliveries
                 SET status = 'pending',
                     attempt_count = 0,
                     next_attempt_at = ?3,
                     claimed_at = NULL,
                     lease_until = NULL,
                     claim_nonce = NULL,
                     last_error = NULL,
                     component_state_json = ?5,
                     dead_lettered_at = NULL,
                     revision = revision + 1,
                     updated_at = ?4
                 WHERE event_id = ?1 AND plugin_id = ?2 AND status = 'dead_letter'",
                params![event_id, plugin_id, next_attempt_at, now_ms, cleaned_state],
            )?;
            if changed == 0 {
                return Err(ensure_delivery_conflict_or_not_found(
                    &tx, event_id, plugin_id,
                ));
            }
            let updated = fetch_delivery(&tx, event_id, plugin_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn renew_delivery_lease(
        &self,
        db: OutboxDb,
        event_id: &str,
        plugin_id: &str,
        claim_nonce: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<OutboxDeliveryRecord, StoreError> {
        validate_non_empty(event_id, "event_id must not be empty")?;
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        validate_non_empty(claim_nonce, "claim_nonce must not be empty")?;
        if now_ms < 0 || lease_duration_ms <= 0 {
            return Err(
                PluginError::Invalid("now_ms must be >= 0 and lease_duration_ms > 0").into(),
            );
        }
        let lease_until = now_ms
            .checked_add(lease_duration_ms)
            .ok_or(PluginError::Invalid("lease_until overflow"))?;
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let changed = tx.execute(
                "UPDATE plugin_event_deliveries
                 SET lease_until = MAX(lease_until, ?4),
                     revision = revision + 1,
                     updated_at = MAX(updated_at, ?5)
                 WHERE event_id = ?1 AND plugin_id = ?2
                   AND status = 'delivering' AND claim_nonce = ?3",
                params![event_id, plugin_id, claim_nonce, lease_until, now_ms],
            )?;
            if changed == 0 {
                return Err(ensure_delivery_conflict_or_not_found(
                    &tx, event_id, plugin_id,
                ));
            }
            let updated = fetch_delivery(&tx, event_id, plugin_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn save_delivery_component_state(
        &self,
        db: OutboxDb,
        event_id: &str,
        plugin_id: &str,
        claim_nonce: &str,
        component_state_json: &str,
        now_ms: i64,
    ) -> Result<OutboxDeliveryRecord, StoreError> {
        validate_non_empty(event_id, "event_id must not be empty")?;
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        validate_non_empty(claim_nonce, "claim_nonce must not be empty")?;
        if now_ms < 0 {
            return Err(PluginError::Invalid("now_ms must be >= 0").into());
        }
        let parsed: serde_json::Value = serde_json::from_str(component_state_json)?;
        if !parsed.is_object() {
            return Err(PluginError::Invalid("component_state_json must be a JSON object").into());
        }
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let changed = tx.execute(
                "UPDATE plugin_event_deliveries
                 SET component_state_json = ?4,
                     revision = revision + 1,
                     updated_at = MAX(updated_at, ?5)
                 WHERE event_id = ?1 AND plugin_id = ?2
                   AND status = 'delivering' AND claim_nonce = ?3",
                params![
                    event_id,
                    plugin_id,
                    claim_nonce,
                    component_state_json,
                    now_ms
                ],
            )?;
            if changed == 0 {
                return Err(ensure_delivery_conflict_or_not_found(
                    &tx, event_id, plugin_id,
                ));
            }
            let updated = fetch_delivery(&tx, event_id, plugin_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn prune_delivered_outbox_events(
        &self,
        db: OutboxDb,
        older_than_ms: i64,
        batch_limit: usize,
    ) -> Result<usize, StoreError> {
        if older_than_ms < 0 {
            return Err(PluginError::Invalid("older_than_ms must be >= 0").into());
        }
        let limit = batch_limit.clamp(1, 500) as i64;
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let mut stmt = tx.prepare(
                "SELECT event_id FROM plugin_event_outbox
                 WHERE fanout_state = 'seeded'
                   AND created_at < ?1
                   AND NOT EXISTS (
                        SELECT 1 FROM plugin_event_deliveries d
                        WHERE d.event_id = plugin_event_outbox.event_id
                          AND d.status IN ('pending', 'delivering', 'dead_letter')
                   )
                 ORDER BY created_at ASC, event_id ASC
                 LIMIT ?2",
            )?;
            let ids: Vec<String> = stmt
                .query_map(params![older_than_ms, limit], |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            let mut deleted = 0usize;
            for event_id in ids {
                tx.execute(
                    "DELETE FROM plugin_event_deliveries WHERE event_id = ?1",
                    params![event_id],
                )?;
                deleted += tx.execute(
                    "DELETE FROM plugin_event_outbox WHERE event_id = ?1",
                    params![event_id],
                )?;
            }
            tx.commit()?;
            Ok(deleted)
        })
    }

    /// Reclaims any rows left in `delivering` state when a single-writer process restarts
    /// after an unclean crash (SIGKILL), resetting them to `pending` immediately without
    /// altering `attempt_count` so retry/dead-letter limits remain honest.
    pub fn reclaim_orphaned_deliveries_on_startup(
        &self,
        db: OutboxDb,
        now_ms: i64,
    ) -> Result<usize, StoreError> {
        if now_ms < 0 {
            return Err(PluginError::Invalid("now_ms must be >= 0").into());
        }
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let changed = tx.execute(
                "UPDATE plugin_event_deliveries
                 SET status = 'pending',
                     next_attempt_at = ?1,
                     claimed_at = NULL,
                     lease_until = NULL,
                     claim_nonce = NULL,
                     revision = revision + 1,
                     updated_at = ?1
                 WHERE status = 'delivering'",
                params![now_ms],
            )?;
            tx.commit()?;
            Ok(changed)
        })
    }
}
