use domain::SubscribeId;
use rusqlite::{Connection, OptionalExtension, params};

use super::types::*;
use super::{parse_ts_id_cursor, validate_non_empty};
use crate::maps::parse_id;
use crate::plugins::{PluginError, PluginPage, paging};
use crate::{Store, StoreError};

const MAX_DEFER_ATTEMPTS: i64 = 10;
const MAX_DEFER_WINDOW_SECS: i64 = 259_200;

const DEF_COLS: &str = "defer_id, subscribe_id, subscribe_generation, policy_revision, enclosure, info_hash, plugin_id, hook_id, reason, attempt_count, first_deferred_at, next_retry_at, max_deadline_at, state, revision, created_at, updated_at";

fn map_defer(row: &rusqlite::Row<'_>) -> rusqlite::Result<SubscribeDeferredCandidateRecord> {
    let state_raw: String = row.get(13)?;
    Ok(SubscribeDeferredCandidateRecord {
        defer_id: row.get(0)?,
        subscribe_id: parse_id(row.get(1)?, 1)?,
        subscribe_generation: row.get(2)?,
        policy_revision: row.get(3)?,
        enclosure: row.get(4)?,
        info_hash: row.get(5)?,
        plugin_id: row.get(6)?,
        hook_id: row.get(7)?,
        reason: row.get(8)?,
        attempt_count: row.get(9)?,
        first_deferred_at: row.get(10)?,
        next_retry_at: row.get(11)?,
        max_deadline_at: row.get(12)?,
        state: state_raw
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        revision: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

fn fetch_binding(
    conn: &Connection,
    subscribe_id: SubscribeId,
) -> Result<Option<SubscribePolicyBinding>, StoreError> {
    conn.query_row(
        "SELECT jobs_sync_generation, policy_revision FROM subscribes WHERE id = ?1",
        params![subscribe_id.to_string()],
        |row| {
            Ok(SubscribePolicyBinding {
                subscribe_id,
                jobs_sync_generation: row.get(0)?,
                policy_revision: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn fetch_defer_for_binding(
    conn: &Connection,
    subscribe_id: SubscribeId,
    generation: &str,
    policy_revision: i64,
    enclosure: &str,
) -> Result<Option<SubscribeDeferredCandidateRecord>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {DEF_COLS} FROM subscribe_deferred_candidates
             WHERE subscribe_id = ?1 AND subscribe_generation = ?2
               AND policy_revision = ?3 AND enclosure = ?4"
        ),
        params![
            subscribe_id.to_string(),
            generation,
            policy_revision,
            enclosure
        ],
        map_defer,
    )
    .optional()
    .map_err(Into::into)
}

fn validate_defer_input(input: &RecordSubscribeDeferInput) -> Result<(), StoreError> {
    validate_non_empty(&input.defer_id, "defer_id must not be empty")?;
    validate_non_empty(
        &input.subscribe_generation,
        "subscribe_generation must not be empty",
    )?;
    validate_non_empty(&input.enclosure, "enclosure must not be empty")?;
    validate_non_empty(&input.hook_id, "hook_id must not be empty")?;
    validate_non_empty(&input.reason, "reason must not be empty")
}

fn update_existing_defer(
    tx: &Connection,
    existing: &SubscribeDeferredCandidateRecord,
    input: &RecordSubscribeDeferInput,
) -> Result<SubscribeDeferredCandidateRecord, StoreError> {
    match existing.state {
        SubscribeDeferState::Rejected => return Err(PluginError::Denied.into()),
        SubscribeDeferState::Deferred => {
            let next_attempt = existing.attempt_count + 1;
            if next_attempt > MAX_DEFER_ATTEMPTS || input.now_utc >= existing.max_deadline_at {
                tx.execute(
                    "UPDATE subscribe_deferred_candidates
                     SET state = 'rejected', hook_id = ?2, plugin_id = ?3,
                         info_hash = COALESCE(?4, info_hash), reason = ?5,
                         revision = revision + 1, updated_at = ?6
                     WHERE defer_id = ?1",
                    params![
                        existing.defer_id,
                        input.hook_id,
                        input.plugin_id,
                        input.info_hash,
                        input.reason,
                        input.now_utc,
                    ],
                )?;
            } else {
                tx.execute(
                    "UPDATE subscribe_deferred_candidates
                     SET attempt_count = ?2, next_retry_at = ?3, reason = ?4,
                         hook_id = ?5, plugin_id = ?6,
                         info_hash = COALESCE(?7, info_hash),
                         revision = revision + 1, updated_at = ?8
                     WHERE defer_id = ?1",
                    params![
                        existing.defer_id,
                        next_attempt,
                        input.next_retry_at,
                        input.reason,
                        input.hook_id,
                        input.plugin_id,
                        input.info_hash,
                        input.now_utc,
                    ],
                )?;
            }
        }
        SubscribeDeferState::Invalidated | SubscribeDeferState::Resolved => {
            let max_deadline = input
                .now_utc
                .checked_add(MAX_DEFER_WINDOW_SECS)
                .ok_or(PluginError::Invalid("defer deadline overflow"))?;
            tx.execute(
                "UPDATE subscribe_deferred_candidates
                 SET hook_id = ?2, plugin_id = ?3, info_hash = ?4, reason = ?5,
                     attempt_count = 1, first_deferred_at = ?6, next_retry_at = ?7,
                     max_deadline_at = ?8, state = 'deferred',
                     revision = revision + 1, updated_at = ?6
                 WHERE defer_id = ?1",
                params![
                    existing.defer_id,
                    input.hook_id,
                    input.plugin_id,
                    input.info_hash,
                    input.reason,
                    input.now_utc,
                    input.next_retry_at,
                    max_deadline,
                ],
            )?;
        }
    }
    fetch_defer_for_binding_opt(tx, input)
}

fn insert_new_defer(
    tx: &Connection,
    sub_str: &str,
    input: &RecordSubscribeDeferInput,
) -> Result<SubscribeDeferredCandidateRecord, StoreError> {
    let max_deadline = input
        .now_utc
        .checked_add(MAX_DEFER_WINDOW_SECS)
        .ok_or(PluginError::Invalid("defer deadline overflow"))?;
    tx.execute(
        "INSERT INTO subscribe_deferred_candidates (
            defer_id, subscribe_id, subscribe_generation, policy_revision,
            enclosure, info_hash, plugin_id, hook_id, reason, attempt_count,
            first_deferred_at, next_retry_at, max_deadline_at, state,
            revision, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12, 'deferred', 1, ?10, ?10)",
        params![
            input.defer_id,
            sub_str,
            input.subscribe_generation,
            input.policy_revision,
            input.enclosure,
            input.info_hash,
            input.plugin_id,
            input.hook_id,
            input.reason,
            input.now_utc,
            input.next_retry_at,
            max_deadline,
        ],
    )?;
    fetch_defer_for_binding_opt(tx, input)
}

fn fetch_defer_for_binding_opt(
    tx: &Connection,
    input: &RecordSubscribeDeferInput,
) -> Result<SubscribeDeferredCandidateRecord, StoreError> {
    fetch_defer_for_binding(
        tx,
        input.subscribe_id,
        &input.subscribe_generation,
        input.policy_revision,
        &input.enclosure,
    )?
    .ok_or_else(|| PluginError::NotFound.into())
}

impl Store {
    pub fn subscribe_policy_binding(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<Option<SubscribePolicyBinding>, StoreError> {
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let row = fetch_binding(&tx, subscribe_id)?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn record_subscribe_defer(
        &self,
        input: &RecordSubscribeDeferInput,
    ) -> Result<SubscribeDeferredCandidateRecord, StoreError> {
        validate_defer_input(input)?;
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let binding = fetch_binding(&tx, input.subscribe_id)?.ok_or(PluginError::NotFound)?;
            if binding.jobs_sync_generation != input.subscribe_generation
                || binding.policy_revision != input.policy_revision
            {
                return Err(PluginError::StaleAuthority.into());
            }
            let sub_str = input.subscribe_id.to_string();
            tx.execute(
                "UPDATE subscribe_deferred_candidates
                 SET state = 'invalidated', revision = revision + 1, updated_at = ?5
                 WHERE subscribe_id = ?1 AND enclosure = ?2 AND state = 'deferred'
                   AND (subscribe_generation != ?3 OR policy_revision != ?4)",
                params![
                    sub_str,
                    input.enclosure,
                    input.subscribe_generation,
                    input.policy_revision,
                    input.now_utc,
                ],
            )?;
            if let Some(existing) = fetch_defer_for_binding(
                &tx,
                input.subscribe_id,
                &input.subscribe_generation,
                input.policy_revision,
                &input.enclosure,
            )? {
                let updated = update_existing_defer(&tx, &existing, input)?;
                tx.commit()?;
                return Ok(updated);
            }
            let created = insert_new_defer(&tx, &sub_str, input)?;
            tx.commit()?;
            Ok(created)
        })
    }

    pub fn get_subscribe_defer(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
    ) -> Result<Option<SubscribeDeferredCandidateRecord>, StoreError> {
        validate_non_empty(enclosure, "enclosure must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let row = tx
                .query_row(
                    "SELECT d.defer_id, d.subscribe_id, d.subscribe_generation, d.policy_revision,
                            d.enclosure, d.info_hash, d.plugin_id, d.hook_id, d.reason,
                            d.attempt_count, d.first_deferred_at, d.next_retry_at,
                            d.max_deadline_at, d.state, d.revision, d.created_at, d.updated_at
                     FROM subscribe_deferred_candidates d
                     JOIN subscribes s ON s.id = d.subscribe_id
                     WHERE d.subscribe_id = ?1 AND d.enclosure = ?2
                     ORDER BY (d.subscribe_generation = s.jobs_sync_generation AND d.policy_revision = s.policy_revision) DESC,
                              d.policy_revision DESC,
                              d.updated_at DESC,
                              d.rowid DESC
                     LIMIT 1",
                    params![subscribe_id.to_string(), enclosure],
                    map_defer,
                )
                .optional()?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn list_due_subscribe_defers(
        &self,
        now_utc: i64,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<SubscribeDeferredCandidateRecord>, StoreError> {
        let cap = paging::api_limit(limit);
        let cursor = parse_ts_id_cursor(after)?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(
                "SELECT d.defer_id, d.subscribe_id, d.subscribe_generation, d.policy_revision,
                        d.enclosure, d.info_hash, d.plugin_id, d.hook_id, d.reason,
                        d.attempt_count, d.first_deferred_at, d.next_retry_at,
                        d.max_deadline_at, d.state, d.revision, d.created_at, d.updated_at
                 FROM subscribe_deferred_candidates d
                 JOIN subscribes s ON s.id = d.subscribe_id
                 WHERE d.state = 'deferred'
                   AND d.subscribe_generation = s.jobs_sync_generation
                   AND d.policy_revision = s.policy_revision
                   AND d.next_retry_at <= ?1
                   AND (?2 IS NULL OR d.next_retry_at > ?2 OR (d.next_retry_at = ?2 AND d.defer_id > ?3))
                 ORDER BY d.next_retry_at ASC, d.defer_id ASC
                 LIMIT ?4",
            )?;
            let items = stmt
                .query_map(
                    params![
                        now_utc,
                        cursor.as_ref().map(|(ts, _)| *ts),
                        cursor.as_ref().map(|(_, id)| id.as_str()),
                        (cap + 1) as i64,
                    ],
                    map_defer,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(items, cap, |r| {
                format!("{}:{}", r.next_retry_at, r.defer_id)
            }))
        })
    }

    pub fn resolve_subscribe_defer(
        &self,
        defer_id: &str,
        expected_revision: i64,
        now_utc: i64,
    ) -> Result<SubscribeDeferredCandidateRecord, StoreError> {
        validate_non_empty(defer_id, "defer_id must not be empty")?;
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let changed = tx.execute(
                "UPDATE subscribe_deferred_candidates
                 SET state = 'resolved', revision = revision + 1, updated_at = ?3
                 WHERE defer_id = ?1 AND revision = ?2 AND state = 'deferred'",
                params![defer_id, expected_revision, now_utc],
            )?;
            if changed == 0 {
                return Err(PluginError::Conflict.into());
            }
            let updated = tx.query_row(
                &format!(
                    "SELECT {DEF_COLS} FROM subscribe_deferred_candidates WHERE defer_id = ?1"
                ),
                params![defer_id],
                map_defer,
            )?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn clear_rejected_subscribe_defer(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        now_utc: i64,
    ) -> Result<bool, StoreError> {
        validate_non_empty(enclosure, "enclosure must not be empty")?;
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let changed = tx.execute(
                "UPDATE subscribe_deferred_candidates
                 SET state = 'invalidated', revision = revision + 1, updated_at = ?3
                 WHERE subscribe_id = ?1 AND enclosure = ?2 AND state = 'rejected'",
                params![subscribe_id.to_string(), enclosure, now_utc],
            )?;
            tx.commit()?;
            Ok(changed > 0)
        })
    }

    pub fn list_subscribe_defers(
        &self,
        subscribe_id: SubscribeId,
        limit: usize,
    ) -> Result<Vec<SubscribeDeferredCandidateRecord>, StoreError> {
        let cap = limit.clamp(1, 100) as i64;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {DEF_COLS} FROM subscribe_deferred_candidates
                 WHERE subscribe_id = ?1
                 ORDER BY updated_at DESC, defer_id DESC
                 LIMIT ?2"
            ))?;
            let items = stmt
                .query_map(params![subscribe_id.to_string(), cap], map_defer)?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(items)
        })
    }
}
