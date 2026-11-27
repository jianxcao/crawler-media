mod emit;
mod queries;
mod sanitize;
mod transitions;
mod types;

pub use emit::{
    emit_outbox_event_in_tx, seed_event_fanout_in_tx, seed_subscriber_deliveries_in_tx,
};
pub use sanitize::sanitize_delivery_error;
pub use types::*;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::StoreError;
use crate::plugins::PluginError;

pub(super) const OUTBOX_COLUMNS: &str = "event_id, schema_version, event_domain, event_kind, entity_type, entity_id, entity_sequence, entity_revision, cause_id, subject, site_id, subscribe_id, library_id, handles_json, payload_json, occurred_at, fanout_state, created_at";

pub(super) const DELIVERY_COLUMNS: &str = "event_id, plugin_id, entity_type, entity_id, entity_sequence, status, attempt_count, next_attempt_at, claimed_at, lease_until, claim_nonce, last_error, component_state_json, delivered_at, dead_lettered_at, revision, created_at, updated_at";

pub(super) fn prefixed_columns(prefix: &str, cols: &str) -> String {
    cols.split(',')
        .map(|col| format!("{prefix}.{}", col.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn validate_non_empty(value: &str, msg: &'static str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(PluginError::Invalid(msg).into());
    }
    Ok(())
}

pub(super) fn validate_opt_non_empty(
    value: Option<&str>,
    msg: &'static str,
) -> Result<(), StoreError> {
    if let Some(raw) = value {
        validate_non_empty(raw, msg)?;
    }
    Ok(())
}

pub(super) fn require_active_tx(tx: &Connection) -> Result<(), StoreError> {
    if tx.is_autocommit() {
        return Err(StoreError::Invalid(
            "outbox event mutation requires an active write transaction".into(),
        ));
    }
    Ok(())
}

pub(super) fn map_outbox_event(
    row: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<OutboxEventRecord> {
    let fanout_raw: String = row.get(offset + 16)?;
    let fanout_state = fanout_raw
        .parse()
        .map_err(|()| rusqlite::Error::InvalidQuery)?;
    Ok(OutboxEventRecord {
        event_id: row.get(offset)?,
        schema_version: row.get(offset + 1)?,
        event_domain: row.get(offset + 2)?,
        event_kind: row.get(offset + 3)?,
        entity_type: row.get(offset + 4)?,
        entity_id: row.get(offset + 5)?,
        entity_sequence: row.get(offset + 6)?,
        entity_revision: row.get(offset + 7)?,
        cause_id: row.get(offset + 8)?,
        subject: row.get(offset + 9)?,
        site_id: row.get(offset + 10)?,
        subscribe_id: row.get(offset + 11)?,
        library_id: row.get(offset + 12)?,
        handles_json: row.get(offset + 13)?,
        payload_json: row.get(offset + 14)?,
        occurred_at: row.get(offset + 15)?,
        fanout_state,
        created_at: row.get(offset + 17)?,
    })
}

pub(super) fn map_delivery(row: &Row<'_>, offset: usize) -> rusqlite::Result<OutboxDeliveryRecord> {
    let status_raw: String = row.get(offset + 5)?;
    let status = status_raw
        .parse()
        .map_err(|()| rusqlite::Error::InvalidQuery)?;
    Ok(OutboxDeliveryRecord {
        event_id: row.get(offset)?,
        plugin_id: row.get(offset + 1)?,
        entity_type: row.get(offset + 2)?,
        entity_id: row.get(offset + 3)?,
        entity_sequence: row.get(offset + 4)?,
        status,
        attempt_count: row.get(offset + 6)?,
        next_attempt_at: row.get(offset + 7)?,
        claimed_at: row.get(offset + 8)?,
        lease_until: row.get(offset + 9)?,
        claim_nonce: row.get(offset + 10)?,
        last_error: row.get(offset + 11)?,
        component_state_json: row.get(offset + 12)?,
        delivered_at: row.get(offset + 13)?,
        dead_lettered_at: row.get(offset + 14)?,
        revision: row.get(offset + 15)?,
        created_at: row.get(offset + 16)?,
        updated_at: row.get(offset + 17)?,
    })
}

pub(super) fn fetch_outbox_event(
    conn: &Connection,
    event_id: &str,
) -> Result<Option<OutboxEventRecord>, StoreError> {
    conn.query_row(
        &format!("SELECT {OUTBOX_COLUMNS} FROM plugin_event_outbox WHERE event_id = ?1"),
        params![event_id],
        |row| map_outbox_event(row, 0),
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn fetch_delivery(
    conn: &Connection,
    event_id: &str,
    plugin_id: &str,
) -> Result<Option<OutboxDeliveryRecord>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {DELIVERY_COLUMNS} FROM plugin_event_deliveries WHERE event_id = ?1 AND plugin_id = ?2"
        ),
        params![event_id, plugin_id],
        |row| map_delivery(row, 0),
    )
    .optional()
    .map_err(Into::into)
}
