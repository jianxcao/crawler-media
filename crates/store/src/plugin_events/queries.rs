use rusqlite::{params, params_from_iter, types::Value};

use super::types::{
    DeadLetterCursor, DeliveryDueCursor, DueOutboxDelivery, OutboxDb, OutboxDeliveryDetail,
    OutboxDeliveryRecord, OutboxDeliveryStatus, OutboxDiagnostics, OutboxEventCursor,
    OutboxEventFilter, OutboxEventRecord,
};
use super::{
    DELIVERY_COLUMNS, OUTBOX_COLUMNS, fetch_delivery, fetch_outbox_event, map_delivery,
    map_outbox_event, prefixed_columns, validate_non_empty, validate_opt_non_empty,
};
use crate::plugins::{PluginError, PluginPage, paging};
use crate::{Store, StoreError};

fn encode_ts_id(ts: i64, id: &str) -> String {
    format!("{ts}:{id}")
}

fn encode_ts_pair(ts: i64, event_id: &str, plugin_id: &str) -> String {
    format!("{ts}:{event_id}:{plugin_id}")
}

fn map_event_and_delivery(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(OutboxEventRecord, OutboxDeliveryRecord)> {
    let event = map_outbox_event(row, 0)?;
    let delivery = map_delivery(row, 18)?;
    Ok((event, delivery))
}

fn build_list_outbox_sql(
    filter: &OutboxEventFilter,
    after: Option<&OutboxEventCursor>,
    limit: usize,
) -> (String, Vec<Value>) {
    let mut conds = Vec::new();
    let mut bind: Vec<Value> = Vec::new();
    for (col, val) in [
        ("event_domain", &filter.event_domain),
        ("event_kind", &filter.event_kind),
        ("entity_type", &filter.entity_type),
        ("entity_id", &filter.entity_id),
    ] {
        if let Some(v) = val {
            bind.push(Value::Text(v.clone()));
            conds.push(format!("{col} = ?{}", bind.len()));
        }
    }
    if let Some(c) = after {
        let p1 = bind.len() + 1;
        let p2 = p1 + 1;
        bind.push(Value::Integer(c.created_at));
        bind.push(Value::Text(c.event_id.clone()));
        conds.push(format!("(created_at, event_id) > (?{p1}, ?{p2})"));
    }
    let where_clause = if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    };
    bind.push(Value::Integer((limit + 1) as i64));
    let sql = format!(
        "SELECT {OUTBOX_COLUMNS} FROM plugin_event_outbox
         {where_clause}
         ORDER BY created_at ASC, event_id ASC
         LIMIT ?{}",
        bind.len()
    );
    (sql, bind)
}

impl Store {
    pub fn get_outbox_event(
        &self,
        db: OutboxDb,
        event_id: &str,
    ) -> Result<Option<OutboxEventRecord>, StoreError> {
        validate_non_empty(event_id, "event_id must not be empty")?;
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let row = fetch_outbox_event(&tx, event_id)?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn list_outbox_events(
        &self,
        db: OutboxDb,
        filter: &OutboxEventFilter,
        after: Option<&OutboxEventCursor>,
        limit: Option<usize>,
    ) -> Result<PluginPage<OutboxEventRecord>, StoreError> {
        let cap = paging::api_limit(limit);
        let (sql, bind) = build_list_outbox_sql(filter, after, cap);
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(bind), |row| map_outbox_event(row, 0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(rows, cap, |r| {
                encode_ts_id(r.created_at, &r.event_id)
            }))
        })
    }

    pub fn list_pending_fanout_events(
        &self,
        db: OutboxDb,
        after: Option<&OutboxEventCursor>,
        limit: Option<usize>,
    ) -> Result<PluginPage<OutboxEventRecord>, StoreError> {
        let cap = paging::api_limit(limit);
        let mut bind: Vec<Value> = Vec::new();
        let cursor_sql = if let Some(c) = after {
            bind.push(Value::Integer(c.created_at));
            bind.push(Value::Text(c.event_id.clone()));
            "AND (created_at, event_id) > (?1, ?2)"
        } else {
            ""
        };
        bind.push(Value::Integer((cap + 1) as i64));
        let sql = format!(
            "SELECT {OUTBOX_COLUMNS} FROM plugin_event_outbox
             WHERE fanout_state = 'pending' {cursor_sql}
             ORDER BY created_at ASC, event_id ASC
             LIMIT ?{}",
            bind.len()
        );
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(bind), |row| map_outbox_event(row, 0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(rows, cap, |r| {
                encode_ts_id(r.created_at, &r.event_id)
            }))
        })
    }

    pub fn list_unseeded_outbox_events(
        &self,
        db: OutboxDb,
        plugin_id: &str,
        after: Option<&OutboxEventCursor>,
        limit: Option<usize>,
    ) -> Result<PluginPage<OutboxEventRecord>, StoreError> {
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        let cap = paging::api_limit(limit);
        let mut bind: Vec<Value> = vec![Value::Text(plugin_id.to_string())];
        let cursor_sql = if let Some(c) = after {
            bind.push(Value::Integer(c.created_at));
            bind.push(Value::Text(c.event_id.clone()));
            "AND (o.created_at, o.event_id) > (?2, ?3)"
        } else {
            ""
        };
        bind.push(Value::Integer((cap + 1) as i64));
        let cols = prefixed_columns("o", OUTBOX_COLUMNS);
        let sql = format!(
            "SELECT {cols} FROM plugin_event_outbox o
             WHERE NOT EXISTS (
                SELECT 1 FROM plugin_event_deliveries d
                WHERE d.event_id = o.event_id AND d.plugin_id = ?1
             ) {cursor_sql}
             ORDER BY o.created_at ASC, o.event_id ASC
             LIMIT ?{}",
            bind.len()
        );
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(bind), |row| map_outbox_event(row, 0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(rows, cap, |r| {
                encode_ts_id(r.created_at, &r.event_id)
            }))
        })
    }

    pub fn list_due_deliveries(
        &self,
        db: OutboxDb,
        plugin_id: Option<&str>,
        now_ms: i64,
        after: Option<&DeliveryDueCursor>,
        limit: Option<usize>,
    ) -> Result<PluginPage<DueOutboxDelivery>, StoreError> {
        validate_opt_non_empty(plugin_id, "plugin_id must not be empty")?;
        if now_ms < 0 {
            return Err(PluginError::Invalid("now_ms must be >= 0").into());
        }
        let cap = paging::api_limit(limit);
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
        let mut bind: Vec<Value> = vec![Value::Integer(now_ms)];
        if let Some(pid) = plugin_id {
            bind.push(Value::Text(pid.to_string()));
            conds.push(format!("d.plugin_id = ?{}", bind.len()));
        }
        if let Some(c) = after {
            let p1 = bind.len() + 1;
            let p2 = p1 + 1;
            let p3 = p2 + 1;
            bind.push(Value::Integer(c.next_attempt_at));
            bind.push(Value::Text(c.event_id.clone()));
            bind.push(Value::Text(c.plugin_id.clone()));
            conds.push(format!(
                "(d.next_attempt_at, d.event_id, d.plugin_id) > (?{p1}, ?{p2}, ?{p3})"
            ));
        }
        bind.push(Value::Integer((cap + 1) as i64));
        let o_cols = prefixed_columns("o", OUTBOX_COLUMNS);
        let d_cols = prefixed_columns("d", DELIVERY_COLUMNS);
        let sql = format!(
            "SELECT {o_cols}, {d_cols}
             FROM plugin_event_deliveries d
             JOIN plugin_event_outbox o ON o.event_id = d.event_id
             WHERE {}
             ORDER BY d.next_attempt_at ASC, d.event_id ASC, d.plugin_id ASC
             LIMIT ?{}",
            conds.join(" AND "),
            bind.len()
        );
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(bind), map_event_and_delivery)?
                .map(|res| res.map(|(event, delivery)| DueOutboxDelivery { event, delivery }))
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(rows, cap, |r| {
                encode_ts_pair(
                    r.delivery.next_attempt_at,
                    &r.delivery.event_id,
                    &r.delivery.plugin_id,
                )
            }))
        })
    }

    pub fn get_delivery_receipt(
        &self,
        db: OutboxDb,
        event_id: &str,
        plugin_id: &str,
    ) -> Result<Option<OutboxDeliveryRecord>, StoreError> {
        validate_non_empty(event_id, "event_id must not be empty")?;
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let row = fetch_delivery(&tx, event_id, plugin_id)?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn list_delivery_receipts_for_event(
        &self,
        db: OutboxDb,
        event_id: &str,
    ) -> Result<Vec<OutboxDeliveryRecord>, StoreError> {
        validate_non_empty(event_id, "event_id must not be empty")?;
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let sql = format!(
                "SELECT {DELIVERY_COLUMNS} FROM plugin_event_deliveries
                 WHERE event_id = ?1 ORDER BY plugin_id ASC"
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt
                .query_map(params![event_id], |row| map_delivery(row, 0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(rows)
        })
    }

    pub fn list_dead_letter_deliveries(
        &self,
        db: OutboxDb,
        plugin_id: Option<&str>,
        after: Option<&DeadLetterCursor>,
        limit: Option<usize>,
    ) -> Result<PluginPage<OutboxDeliveryDetail>, StoreError> {
        validate_opt_non_empty(plugin_id, "plugin_id must not be empty")?;
        let cap = paging::api_limit(limit);
        let mut conds = vec!["d.status = 'dead_letter'".to_string()];
        let mut bind: Vec<Value> = Vec::new();
        if let Some(pid) = plugin_id {
            bind.push(Value::Text(pid.to_string()));
            conds.push(format!("d.plugin_id = ?{}", bind.len()));
        }
        if let Some(c) = after {
            let p1 = bind.len() + 1;
            let p2 = p1 + 1;
            let p3 = p2 + 1;
            bind.push(Value::Integer(c.dead_lettered_at));
            bind.push(Value::Text(c.event_id.clone()));
            bind.push(Value::Text(c.plugin_id.clone()));
            conds.push(format!(
                "(d.dead_lettered_at < ?{p1} OR (d.dead_lettered_at = ?{p1} AND (d.event_id > ?{p2} OR (d.event_id = ?{p2} AND d.plugin_id > ?{p3}))))"
            ));
        }
        bind.push(Value::Integer((cap + 1) as i64));
        let o_cols = prefixed_columns("o", OUTBOX_COLUMNS);
        let d_cols = prefixed_columns("d", DELIVERY_COLUMNS);
        let sql = format!(
            "SELECT {o_cols}, {d_cols}
             FROM plugin_event_deliveries d
             JOIN plugin_event_outbox o ON o.event_id = d.event_id
             WHERE {}
             ORDER BY d.dead_lettered_at DESC, d.event_id ASC, d.plugin_id ASC
             LIMIT ?{}",
            conds.join(" AND "),
            bind.len()
        );
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(bind), map_event_and_delivery)?
                .map(|res| res.map(|(event, delivery)| OutboxDeliveryDetail { event, delivery }))
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(rows, cap, |r| {
                encode_ts_pair(
                    r.delivery.dead_lettered_at.unwrap_or(0),
                    &r.delivery.event_id,
                    &r.delivery.plugin_id,
                )
            }))
        })
    }

    pub fn get_outbox_diagnostics(
        &self,
        db: OutboxDb,
        now_ms: i64,
    ) -> Result<OutboxDiagnostics, StoreError> {
        if now_ms < 0 {
            return Err(PluginError::Invalid("now_ms must be >= 0").into());
        }
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let outbox_total: i64 =
                tx.query_row("SELECT COUNT(*) FROM plugin_event_outbox", [], |r| r.get(0))?;
            let pending_fanout_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM plugin_event_outbox WHERE fanout_state = 'pending'",
                [],
                |r| r.get(0),
            )?;
            let (pending_count, due_count, delivering_count, delivered_count, dead_letter_count): (
                i64,
                i64,
                i64,
                i64,
                i64,
            ) = tx.query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN (status = 'pending' AND next_attempt_at <= ?1)
                                        OR (status = 'delivering' AND lease_until <= ?1)
                                      THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'delivering' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'delivered' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'dead_letter' THEN 1 ELSE 0 END), 0)
                 FROM plugin_event_deliveries",
                params![now_ms],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let oldest_pending_created_at: Option<i64> = tx.query_row(
                "SELECT MIN(o.created_at)
                 FROM plugin_event_outbox o
                 WHERE o.fanout_state = 'pending'
                    OR EXISTS (
                        SELECT 1 FROM plugin_event_deliveries d
                        WHERE d.event_id = o.event_id AND d.status IN ('pending', 'delivering')
                    )",
                [],
                |r| r.get(0),
            )?;
            tx.commit()?;
            let lag_ms = oldest_pending_created_at.map(|ts| now_ms.saturating_sub(ts).max(0));
            Ok(OutboxDiagnostics {
                db,
                outbox_total: u64::try_from(outbox_total).unwrap_or(0),
                pending_fanout_count: u64::try_from(pending_fanout_count).unwrap_or(0),
                pending_count: u64::try_from(pending_count).unwrap_or(0),
                due_count: u64::try_from(due_count).unwrap_or(0),
                delivering_count: u64::try_from(delivering_count).unwrap_or(0),
                delivered_count: u64::try_from(delivered_count).unwrap_or(0),
                dead_letter_count: u64::try_from(dead_letter_count).unwrap_or(0),
                oldest_pending_created_at,
                lag_ms,
            })
        })
    }

    pub fn list_plugin_event_deliveries(
        &self,
        db: OutboxDb,
        plugin_id: &str,
        status: Option<OutboxDeliveryStatus>,
        after: Option<&DeadLetterCursor>,
        limit: Option<usize>,
    ) -> Result<PluginPage<OutboxDeliveryDetail>, StoreError> {
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        let cap = paging::api_limit(limit);
        let mut conds = vec!["d.plugin_id = ?1".to_string()];
        let mut bind: Vec<Value> = vec![Value::Text(plugin_id.to_string())];
        if let Some(st) = status {
            bind.push(Value::Text(st.as_str().to_string()));
            conds.push(format!("d.status = ?{}", bind.len()));
        }
        if let Some(c) = after {
            let p1 = bind.len() + 1;
            let p2 = p1 + 1;
            bind.push(Value::Integer(c.dead_lettered_at));
            bind.push(Value::Text(c.event_id.clone()));
            conds.push(format!(
                "(d.updated_at < ?{p1} OR (d.updated_at = ?{p1} AND d.event_id < ?{p2}))"
            ));
        }
        bind.push(Value::Integer((cap + 1) as i64));
        let o_cols = prefixed_columns("o", OUTBOX_COLUMNS);
        let d_cols = prefixed_columns("d", DELIVERY_COLUMNS);
        let sql = format!(
            "SELECT {o_cols}, {d_cols}
             FROM plugin_event_deliveries d
             JOIN plugin_event_outbox o ON o.event_id = d.event_id
             WHERE {}
             ORDER BY d.updated_at DESC, d.event_id DESC
             LIMIT ?{}",
            conds.join(" AND "),
            bind.len()
        );
        self.with_read(db.into(), |conn| {
            let tx = crate::read_tx(conn)?;
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(bind), map_event_and_delivery)?
                .map(|res| res.map(|(event, delivery)| OutboxDeliveryDetail { event, delivery }))
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(rows, cap, |r| {
                encode_ts_pair(
                    r.delivery.updated_at,
                    &r.delivery.event_id,
                    &r.delivery.plugin_id,
                )
            }))
        })
    }
}
