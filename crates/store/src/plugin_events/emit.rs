use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::types::{
    EmitOutboxEventInput, OutboxDb, OutboxEventRecord, SeedDeliveryInput,
    SeedSubscriberDeliveriesInput,
};
use super::{fetch_outbox_event, require_active_tx, validate_non_empty, validate_opt_non_empty};
use crate::plugins::PluginError;
use crate::{App, LibraryConnection, Store, StoreError, SubscribeConnection};

fn validate_emit_input(input: &EmitOutboxEventInput) -> Result<(), StoreError> {
    validate_non_empty(&input.event_id, "event_id must not be empty")?;
    validate_non_empty(&input.event_domain, "event_domain must not be empty")?;
    validate_non_empty(&input.event_kind, "event_kind must not be empty")?;
    validate_non_empty(&input.entity_type, "entity_type must not be empty")?;
    validate_non_empty(&input.entity_id, "entity_id must not be empty")?;
    validate_non_empty(&input.cause_id, "cause_id must not be empty")?;
    validate_non_empty(&input.subject, "subject must not be empty")?;
    validate_opt_non_empty(input.site_id.as_deref(), "site_id must not be empty")?;
    validate_opt_non_empty(
        input.subscribe_id.as_deref(),
        "subscribe_id must not be empty",
    )?;
    validate_opt_non_empty(input.library_id.as_deref(), "library_id must not be empty")?;
    if input.schema_version <= 0 {
        return Err(PluginError::Invalid("schema_version must be > 0").into());
    }
    if input.entity_revision < 0 {
        return Err(PluginError::Invalid("entity_revision must be >= 0").into());
    }
    if input.occurred_at < 0 || input.created_at < 0 {
        return Err(PluginError::Invalid("timestamps must be >= 0").into());
    }
    if let Some(seq) = input.entity_sequence
        && seq <= 0
    {
        return Err(PluginError::Invalid("entity_sequence must be > 0").into());
    }
    let handles: Value = serde_json::from_str(&input.handles_json)?;
    if !handles.is_array() {
        return Err(PluginError::Invalid("handles_json must be a JSON array").into());
    }
    let payload: Value = serde_json::from_str(&input.payload_json)?;
    if !payload.is_object() {
        return Err(PluginError::Invalid("payload_json must be a JSON object").into());
    }
    Ok(())
}

fn allocate_entity_sequence(
    tx: &Connection,
    entity_type: &str,
    entity_id: &str,
    explicit: Option<i64>,
) -> Result<i64, StoreError> {
    match explicit {
        None => tx
            .query_row(
                "INSERT INTO plugin_event_entity_sequences(entity_type, entity_id, last_sequence)
                 VALUES (?1, ?2, 1)
                 ON CONFLICT(entity_type, entity_id)
                 DO UPDATE SET last_sequence = last_sequence + 1
                 RETURNING last_sequence",
                params![entity_type, entity_id],
                |row| row.get(0),
            )
            .map_err(Into::into),
        Some(seq) => {
            let current: Option<i64> = tx
                .query_row(
                    "SELECT last_sequence FROM plugin_event_entity_sequences
                     WHERE entity_type = ?1 AND entity_id = ?2",
                    params![entity_type, entity_id],
                    |row| row.get(0),
                )
                .optional()?;
            if current.is_some_and(|prev| seq <= prev) {
                return Err(PluginError::Conflict.into());
            }
            let updated = tx.execute(
                "INSERT INTO plugin_event_entity_sequences(entity_type, entity_id, last_sequence)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(entity_type, entity_id)
                 DO UPDATE SET last_sequence = excluded.last_sequence
                 WHERE excluded.last_sequence > last_sequence",
                params![entity_type, entity_id, seq],
            )?;
            if updated == 0 {
                return Err(PluginError::Conflict.into());
            }
            Ok(seq)
        }
    }
}

fn is_constraint_err(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                ..
            },
            _
        )
    )
}

/// Inserts a durable outbox event inside an already-open write transaction.
/// Rolled-back transactions leave zero outbox or sequence rows.
pub fn emit_outbox_event_in_tx(
    tx: &Connection,
    input: &EmitOutboxEventInput,
) -> Result<OutboxEventRecord, StoreError> {
    require_active_tx(tx)?;
    validate_emit_input(input)?;
    if fetch_outbox_event(tx, &input.event_id)?.is_some() {
        return Err(PluginError::Conflict.into());
    }
    let sequence = allocate_entity_sequence(
        tx,
        &input.entity_type,
        &input.entity_id,
        input.entity_sequence,
    )?;
    let res = tx.execute(
        "INSERT INTO plugin_event_outbox(
            event_id, schema_version, event_domain, event_kind,
            entity_type, entity_id, entity_sequence, entity_revision,
            cause_id, subject, site_id, subscribe_id, library_id,
            handles_json, payload_json, occurred_at, fanout_state, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, 'pending', ?17)",
        params![
            input.event_id,
            input.schema_version,
            input.event_domain,
            input.event_kind,
            input.entity_type,
            input.entity_id,
            sequence,
            input.entity_revision,
            input.cause_id,
            input.subject,
            input.site_id,
            input.subscribe_id,
            input.library_id,
            input.handles_json,
            input.payload_json,
            input.occurred_at,
            input.created_at,
        ],
    );
    if let Err(err) = res {
        if is_constraint_err(&err) {
            return Err(PluginError::Conflict.into());
        }
        return Err(err.into());
    }
    fetch_outbox_event(tx, &input.event_id)?.ok_or_else(|| PluginError::NotFound.into())
}

fn insert_delivery_seed(
    tx: &Connection,
    event: &OutboxEventRecord,
    plugin_id: &str,
    next_attempt_at: i64,
    now_ms: i64,
) -> Result<usize, StoreError> {
    validate_non_empty(plugin_id, "plugin_id must not be empty")?;
    if next_attempt_at < 0 || now_ms < 0 {
        return Err(PluginError::Invalid("timestamps must be >= 0").into());
    }
    tx.execute(
        "INSERT OR IGNORE INTO plugin_event_deliveries(
            event_id, plugin_id, entity_type, entity_id, entity_sequence,
            status, attempt_count, next_attempt_at, claimed_at, lease_until,
            claim_nonce, last_error, delivered_at, dead_lettered_at,
            revision, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, NULL, NULL, NULL, NULL, NULL, NULL, 1, ?7, ?7)",
        params![
            event.event_id,
            plugin_id,
            event.entity_type,
            event.entity_id,
            event.entity_sequence,
            next_attempt_at,
            now_ms,
        ],
    )
    .map_err(Into::into)
}

/// Seeds per-subscriber delivery rows in an active write transaction and marks
/// the corresponding outbox rows `fanout_state = 'seeded'`.
pub fn seed_subscriber_deliveries_in_tx(
    tx: &Connection,
    seeds: &[SeedDeliveryInput],
    now_ms: i64,
) -> Result<usize, StoreError> {
    require_active_tx(tx)?;
    if now_ms < 0 {
        return Err(PluginError::Invalid("now_ms must be >= 0").into());
    }
    let mut inserted = 0usize;
    for seed in seeds {
        validate_non_empty(&seed.event_id, "event_id must not be empty")?;
        let event = fetch_outbox_event(tx, &seed.event_id)?.ok_or(PluginError::NotFound)?;
        inserted +=
            insert_delivery_seed(tx, &event, &seed.plugin_id, seed.next_attempt_at, now_ms)?;
        tx.execute(
            "UPDATE plugin_event_outbox SET fanout_state = 'seeded'
             WHERE event_id = ?1 AND fanout_state = 'pending'",
            params![seed.event_id],
        )?;
    }
    Ok(inserted)
}

/// Seeds all subscriber plugins for a single event (including 0 plugins) and marks
/// `fanout_state = 'seeded'` atomically inside the active write transaction.
pub fn seed_event_fanout_in_tx(
    tx: &Connection,
    input: &SeedSubscriberDeliveriesInput,
) -> Result<usize, StoreError> {
    require_active_tx(tx)?;
    validate_non_empty(&input.event_id, "event_id must not be empty")?;
    if input.next_attempt_at < 0 || input.now_ms < 0 {
        return Err(PluginError::Invalid("timestamps must be >= 0").into());
    }
    let event = fetch_outbox_event(tx, &input.event_id)?.ok_or(PluginError::NotFound)?;
    let mut inserted = 0usize;
    for plugin_id in &input.plugin_ids {
        inserted +=
            insert_delivery_seed(tx, &event, plugin_id, input.next_attempt_at, input.now_ms)?;
    }
    tx.execute(
        "UPDATE plugin_event_outbox SET fanout_state = 'seeded'
         WHERE event_id = ?1 AND fanout_state = 'pending'",
        params![input.event_id],
    )?;
    Ok(inserted)
}

impl App<'_> {
    pub fn emit_outbox_event(
        &self,
        input: &EmitOutboxEventInput,
    ) -> Result<OutboxEventRecord, StoreError> {
        emit_outbox_event_in_tx(self.app, input)
    }
}

impl SubscribeConnection<'_> {
    pub fn emit_outbox_event(
        &self,
        input: &EmitOutboxEventInput,
    ) -> Result<OutboxEventRecord, StoreError> {
        emit_outbox_event_in_tx(self.subscribe, input)
    }
}

impl LibraryConnection<'_> {
    pub fn emit_outbox_event(
        &self,
        input: &EmitOutboxEventInput,
    ) -> Result<OutboxEventRecord, StoreError> {
        emit_outbox_event_in_tx(self.library, input)
    }
}

impl Store {
    pub fn emit_outbox_event(
        &self,
        db: OutboxDb,
        input: &EmitOutboxEventInput,
    ) -> Result<OutboxEventRecord, StoreError> {
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let record = emit_outbox_event_in_tx(&tx, input)?;
            tx.commit()?;
            Ok(record)
        })
    }

    pub fn seed_subscriber_deliveries(
        &self,
        db: OutboxDb,
        seeds: &[SeedDeliveryInput],
        now_ms: i64,
    ) -> Result<usize, StoreError> {
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let count = seed_subscriber_deliveries_in_tx(&tx, seeds, now_ms)?;
            tx.commit()?;
            Ok(count)
        })
    }

    pub fn seed_event_fanout(
        &self,
        db: OutboxDb,
        input: &SeedSubscriberDeliveriesInput,
    ) -> Result<usize, StoreError> {
        self.with_writer(db.into(), |conn| {
            let tx = crate::write_tx(conn)?;
            let count = seed_event_fanout_in_tx(&tx, input)?;
            tx.commit()?;
            Ok(count)
        })
    }
}
