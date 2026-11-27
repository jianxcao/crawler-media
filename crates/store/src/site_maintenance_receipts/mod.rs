mod types;
pub use types::*;

use crate::maps::parse_id;
use crate::plugins::{PluginError, PluginPage, paging};
use crate::{App, Store, StoreError};
use domain::SiteId;
use rusqlite::{Connection, OptionalExtension, params};

const COLUMNS: &str = "receipt_id, site_id, action, window_key, timezone, window_start_utc, window_end_utc, plugin_id, provider_id, generation, credential_epoch, state, outcome, evidence, revision, created_at, updated_at";

fn map_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<SiteMaintenanceReceipt> {
    let action_raw: String = row.get(2)?;
    let state_raw: String = row.get(11)?;
    let action = action_raw
        .parse()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let state = state_raw
        .parse()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(SiteMaintenanceReceipt {
        receipt_id: row.get(0)?,
        site_id: parse_id(row.get(1)?, 1)?,
        action,
        window_key: row.get(3)?,
        timezone: row.get(4)?,
        window_start_utc: row.get(5)?,
        window_end_utc: row.get(6)?,
        plugin_id: row.get(7)?,
        provider_id: row.get(8)?,
        generation: row.get(9)?,
        credential_epoch: row.get(10)?,
        state,
        outcome: row.get(12)?,
        evidence: row.get(13)?,
        revision: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

fn validate_non_empty(value: &str, msg: &'static str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(PluginError::Invalid(msg).into());
    }
    Ok(())
}

fn validate_claim(input: &ClaimSiteMaintenanceReceipt) -> Result<(), StoreError> {
    validate_non_empty(&input.window_key, "window_key must not be empty")?;
    validate_non_empty(&input.timezone, "timezone must not be empty")?;
    validate_non_empty(&input.plugin_id, "plugin_id must not be empty")?;
    validate_non_empty(&input.provider_id, "provider_id must not be empty")?;
    validate_non_empty(
        &input.credential_epoch,
        "credential_epoch must not be empty",
    )?;
    if input.generation < 0 {
        return Err(PluginError::Invalid("generation must be >= 0").into());
    }
    if input.window_end_utc <= input.window_start_utc {
        return Err(PluginError::Invalid("window_end_utc must exceed window_start_utc").into());
    }
    Ok(())
}

fn blocking_result(row: SiteMaintenanceReceipt) -> Result<SiteMaintenanceClaimResult, StoreError> {
    match row.state {
        SiteMaintenanceState::Completed => Ok(SiteMaintenanceClaimResult::AlreadyCompleted(row)),
        SiteMaintenanceState::Unknown => Ok(SiteMaintenanceClaimResult::BlockedUnknown(row)),
        SiteMaintenanceState::Pending => Ok(SiteMaintenanceClaimResult::InFlight(row)),
        SiteMaintenanceState::Failed => Err(PluginError::Conflict.into()),
    }
}

fn fetch_by_id(
    tx: &Connection,
    receipt_id: &str,
) -> Result<Option<SiteMaintenanceReceipt>, StoreError> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM site_maintenance_receipts WHERE receipt_id = ?1"),
        params![receipt_id],
        map_receipt,
    )
    .optional()
    .map_err(Into::into)
}

fn fetch_for_window(
    tx: &Connection,
    site_id: SiteId,
    action: SiteMaintenanceAction,
    window_key: &str,
) -> Result<Option<SiteMaintenanceReceipt>, StoreError> {
    tx.query_row(
        &format!(
            "SELECT {COLUMNS} FROM site_maintenance_receipts
             WHERE site_id = ?1 AND action = ?2 AND window_key = ?3"
        ),
        params![site_id.to_string(), action.as_str(), window_key],
        map_receipt,
    )
    .optional()
    .map_err(Into::into)
}

fn fetch_overlapping_active(
    tx: &Connection,
    site_id: SiteId,
    action: SiteMaintenanceAction,
    now_utc: i64,
    exclude_receipt_id: Option<&str>,
) -> Result<Option<SiteMaintenanceReceipt>, StoreError> {
    tx.query_row(
        &format!(
            "SELECT {COLUMNS} FROM site_maintenance_receipts
             WHERE site_id = ?1
               AND action = ?2
               AND (
                    state IN ('pending', 'completed')
                    OR (state = 'unknown' AND ?2 = 'check-in')
               )
               AND window_end_utc > ?3
               AND (?4 IS NULL OR receipt_id != ?4)
             ORDER BY CASE state WHEN 'unknown' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END,
                      window_end_utc DESC,
                      receipt_id DESC
             LIMIT 1"
        ),
        params![
            site_id.to_string(),
            action.as_str(),
            now_utc,
            exclude_receipt_id
        ],
        map_receipt,
    )
    .optional()
    .map_err(Into::into)
}

fn parse_ts_id_cursor(after: Option<&str>) -> Result<Option<(i64, String)>, StoreError> {
    let Some(raw) = after else {
        return Ok(None);
    };
    let Some((ts_raw, id)) = raw.split_once(':') else {
        return Err(PluginError::Invalid("invalid receipt cursor").into());
    };
    let ts: i64 = ts_raw
        .parse()
        .map_err(|_| PluginError::Invalid("invalid receipt cursor timestamp"))?;
    if id.trim().is_empty() {
        return Err(PluginError::Invalid("invalid receipt cursor id").into());
    }
    Ok(Some((ts, id.to_string())))
}

impl App<'_> {
    pub fn claim_site_maintenance_receipt(
        &self,
        input: &ClaimSiteMaintenanceReceipt,
    ) -> Result<SiteMaintenanceClaimResult, StoreError> {
        validate_claim(input)?;
        let tx = crate::write_tx(self.app)?;
        let site_exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM sites WHERE id = ?1",
                params![input.site_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if site_exists.is_none() {
            return Err(StoreError::Missing(format!("site {}", input.site_id)));
        }

        let existing = fetch_for_window(&tx, input.site_id, input.action, &input.window_key)?;
        if let Some(row) = existing {
            let reclaimable = row.state == SiteMaintenanceState::Failed
                || (input.action == SiteMaintenanceAction::VerifySession
                    && row.state == SiteMaintenanceState::Unknown);
            if !reclaimable {
                tx.commit()?;
                return blocking_result(row);
            }
            if let Some(overlap) = fetch_overlapping_active(
                &tx,
                input.site_id,
                input.action,
                input.now_utc,
                Some(&row.receipt_id),
            )? {
                tx.commit()?;
                return blocking_result(overlap);
            }
            let updated = tx.execute(
                "UPDATE site_maintenance_receipts
                 SET timezone = ?2, window_start_utc = ?3, window_end_utc = ?4,
                     plugin_id = ?5, provider_id = ?6, generation = ?7,
                     credential_epoch = ?8, state = 'pending', outcome = NULL,
                     evidence = NULL, revision = revision + 1, updated_at = ?9
                 WHERE receipt_id = ?1 AND revision = ?10 AND state = ?11",
                params![
                    row.receipt_id,
                    input.timezone,
                    input.window_start_utc,
                    input.window_end_utc,
                    input.plugin_id,
                    input.provider_id,
                    input.generation,
                    input.credential_epoch,
                    input.now_utc,
                    row.revision,
                    row.state.as_str(),
                ],
            )?;
            if updated == 0 {
                return Err(PluginError::Conflict.into());
            }
            let claimed = fetch_by_id(&tx, &row.receipt_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            return Ok(SiteMaintenanceClaimResult::Claimed(claimed));
        }

        if let Some(overlap) =
            fetch_overlapping_active(&tx, input.site_id, input.action, input.now_utc, None)?
        {
            tx.commit()?;
            return blocking_result(overlap);
        }

        let receipt_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO site_maintenance_receipts (
                receipt_id, site_id, action, window_key, timezone,
                window_start_utc, window_end_utc, plugin_id, provider_id,
                generation, credential_epoch, state, outcome, evidence,
                revision, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', NULL, NULL, 1, ?12, ?12)",
            params![
                receipt_id,
                input.site_id.to_string(),
                input.action.as_str(),
                input.window_key,
                input.timezone,
                input.window_start_utc,
                input.window_end_utc,
                input.plugin_id,
                input.provider_id,
                input.generation,
                input.credential_epoch,
                input.now_utc,
            ],
        )?;
        let claimed = fetch_by_id(&tx, &receipt_id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(SiteMaintenanceClaimResult::Claimed(claimed))
    }

    pub fn finalize_site_maintenance_receipt(
        &self,
        input: &FinalizeSiteMaintenanceReceipt,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        self.finalize_site_maintenance_receipt_with_outbox(input, None)
    }

    pub fn finalize_site_maintenance_receipt_with_outbox(
        &self,
        input: &FinalizeSiteMaintenanceReceipt,
        outbox: Option<&crate::plugin_events::EmitOutboxEventInput>,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        validate_non_empty(&input.receipt_id, "receipt_id must not be empty")?;
        if input.expected_revision <= 0 {
            return Err(PluginError::Invalid("expected_revision must be > 0").into());
        }
        if !matches!(
            input.state,
            SiteMaintenanceState::Completed
                | SiteMaintenanceState::Unknown
                | SiteMaintenanceState::Failed
        ) {
            return Err(PluginError::Invalid(
                "finalize state must be completed, unknown, or failed",
            )
            .into());
        }
        let tx = crate::write_tx(self.app)?;
        let changed = tx.execute(
            "UPDATE site_maintenance_receipts
             SET state = ?3, outcome = ?4, evidence = ?5,
                 revision = revision + 1, updated_at = ?6
             WHERE receipt_id = ?1 AND revision = ?2 AND state = 'pending'",
            params![
                input.receipt_id,
                input.expected_revision,
                input.state.as_str(),
                input.outcome,
                input.evidence,
                input.now_utc,
            ],
        )?;
        if changed == 0 {
            let exists = fetch_by_id(&tx, &input.receipt_id)?.is_some();
            return Err(if exists {
                PluginError::Conflict
            } else {
                PluginError::NotFound
            }
            .into());
        }
        if let Some(ev) = outbox {
            crate::plugin_events::emit_outbox_event_in_tx(&tx, ev)?;
        }
        let updated = fetch_by_id(&tx, &input.receipt_id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn mark_site_maintenance_receipt_unknown(
        &self,
        receipt_id: &str,
        expected_revision: i64,
        outcome: Option<&str>,
        evidence: Option<&str>,
        now_utc: i64,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        self.finalize_site_maintenance_receipt(&FinalizeSiteMaintenanceReceipt {
            receipt_id: receipt_id.to_string(),
            expected_revision,
            state: SiteMaintenanceState::Unknown,
            outcome: outcome.map(str::to_string),
            evidence: evidence.map(str::to_string),
            now_utc,
        })
    }

    pub fn resolve_site_maintenance_receipt(
        &self,
        input: &ResolveSiteMaintenanceReceipt,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        self.resolve_site_maintenance_receipt_with_outbox(input, None)
    }

    pub fn resolve_site_maintenance_receipt_with_outbox(
        &self,
        input: &ResolveSiteMaintenanceReceipt,
        outbox: Option<&crate::plugin_events::EmitOutboxEventInput>,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        validate_non_empty(&input.receipt_id, "receipt_id must not be empty")?;
        if input.expected_revision <= 0 {
            return Err(PluginError::Invalid("expected_revision must be > 0").into());
        }
        if !matches!(
            input.resolved_state,
            SiteMaintenanceState::Completed | SiteMaintenanceState::Failed
        ) {
            return Err(PluginError::Invalid("resolved_state must be completed or failed").into());
        }
        let tx = crate::write_tx(self.app)?;
        let changed = tx.execute(
            "UPDATE site_maintenance_receipts
             SET state = ?3, outcome = ?4, evidence = ?5,
                 revision = revision + 1, updated_at = ?6
             WHERE receipt_id = ?1 AND revision = ?2 AND state = 'unknown'",
            params![
                input.receipt_id,
                input.expected_revision,
                input.resolved_state.as_str(),
                input.outcome,
                input.evidence,
                input.now_utc,
            ],
        )?;
        if changed == 0 {
            let exists = fetch_by_id(&tx, &input.receipt_id)?.is_some();
            return Err(if exists {
                PluginError::Conflict
            } else {
                PluginError::NotFound
            }
            .into());
        }
        if let Some(ev) = outbox {
            crate::plugin_events::emit_outbox_event_in_tx(&tx, ev)?;
        }
        let updated = fetch_by_id(&tx, &input.receipt_id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn get_site_maintenance_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<SiteMaintenanceReceipt>, StoreError> {
        validate_non_empty(receipt_id, "receipt_id must not be empty")?;
        let tx = crate::read_tx(self.app)?;
        let row = fetch_by_id(&tx, receipt_id)?;
        tx.commit()?;
        Ok(row)
    }

    pub fn get_site_maintenance_receipt_for_window(
        &self,
        site_id: SiteId,
        action: SiteMaintenanceAction,
        window_key: &str,
    ) -> Result<Option<SiteMaintenanceReceipt>, StoreError> {
        validate_non_empty(window_key, "window_key must not be empty")?;
        let tx = crate::read_tx(self.app)?;
        let row = fetch_for_window(&tx, site_id, action, window_key)?;
        tx.commit()?;
        Ok(row)
    }

    pub fn list_site_maintenance_receipts(
        &self,
        filter: &SiteMaintenanceReceiptFilter,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<SiteMaintenanceReceipt>, StoreError> {
        let cap = paging::api_limit(limit);
        let cursor = parse_ts_id_cursor(after)?;
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {COLUMNS} FROM site_maintenance_receipts
             WHERE (?1 IS NULL OR site_id = ?1)
               AND (?2 IS NULL OR action = ?2)
               AND (?3 IS NULL OR state = ?3)
               AND (?4 IS NULL OR updated_at < ?4 OR (updated_at = ?4 AND receipt_id < ?5))
             ORDER BY updated_at DESC, receipt_id DESC
             LIMIT ?6"
        ))?;
        let items = stmt
            .query_map(
                params![
                    filter.site_id.map(|id| id.to_string()),
                    filter.action.map(SiteMaintenanceAction::as_str),
                    filter.state.map(SiteMaintenanceState::as_str),
                    cursor.as_ref().map(|(ts, _)| *ts),
                    cursor.as_ref().map(|(_, id)| id.as_str()),
                    (cap + 1) as i64,
                ],
                map_receipt,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(paging::paginate(items, cap, |r| {
            format!("{}:{}", r.updated_at, r.receipt_id)
        }))
    }

    pub fn list_unresolved_site_maintenance_receipts(
        &self,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<SiteMaintenanceReceipt>, StoreError> {
        let cap = paging::api_limit(limit);
        let cursor = parse_ts_id_cursor(after)?;
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {COLUMNS} FROM site_maintenance_receipts
             WHERE state IN ('pending', 'unknown')
               AND (?1 IS NULL OR updated_at > ?1 OR (updated_at = ?1 AND receipt_id > ?2))
             ORDER BY updated_at ASC, receipt_id ASC
             LIMIT ?3"
        ))?;
        let items = stmt
            .query_map(
                params![
                    cursor.as_ref().map(|(ts, _)| *ts),
                    cursor.as_ref().map(|(_, id)| id.as_str()),
                    (cap + 1) as i64,
                ],
                map_receipt,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(paging::paginate(items, cap, |r| {
            format!("{}:{}", r.updated_at, r.receipt_id)
        }))
    }
}

impl Store {
    pub fn claim_site_maintenance_receipt(
        &self,
        input: &ClaimSiteMaintenanceReceipt,
    ) -> Result<SiteMaintenanceClaimResult, StoreError> {
        self.with_app(|app| app.claim_site_maintenance_receipt(input))
    }

    pub fn finalize_site_maintenance_receipt(
        &self,
        input: &FinalizeSiteMaintenanceReceipt,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        self.with_app(|app| app.finalize_site_maintenance_receipt(input))
    }

    pub fn finalize_site_maintenance_receipt_with_outbox(
        &self,
        input: &FinalizeSiteMaintenanceReceipt,
        outbox: Option<&crate::plugin_events::EmitOutboxEventInput>,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        self.with_app(|app| app.finalize_site_maintenance_receipt_with_outbox(input, outbox))
    }

    pub fn mark_site_maintenance_receipt_unknown(
        &self,
        receipt_id: &str,
        expected_revision: i64,
        outcome: Option<&str>,
        evidence: Option<&str>,
        now_utc: i64,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        self.with_app(|app| {
            app.mark_site_maintenance_receipt_unknown(
                receipt_id,
                expected_revision,
                outcome,
                evidence,
                now_utc,
            )
        })
    }

    pub fn resolve_site_maintenance_receipt(
        &self,
        input: &ResolveSiteMaintenanceReceipt,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        self.with_app(|app| app.resolve_site_maintenance_receipt(input))
    }

    pub fn resolve_site_maintenance_receipt_with_outbox(
        &self,
        input: &ResolveSiteMaintenanceReceipt,
        outbox: Option<&crate::plugin_events::EmitOutboxEventInput>,
    ) -> Result<SiteMaintenanceReceipt, StoreError> {
        self.with_app(|app| app.resolve_site_maintenance_receipt_with_outbox(input, outbox))
    }

    pub fn get_site_maintenance_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<SiteMaintenanceReceipt>, StoreError> {
        self.with_app_read(|app| app.get_site_maintenance_receipt(receipt_id))
    }

    pub fn get_site_maintenance_receipt_for_window(
        &self,
        site_id: SiteId,
        action: SiteMaintenanceAction,
        window_key: &str,
    ) -> Result<Option<SiteMaintenanceReceipt>, StoreError> {
        self.with_app_read(|app| {
            app.get_site_maintenance_receipt_for_window(site_id, action, window_key)
        })
    }

    pub fn list_site_maintenance_receipts(
        &self,
        filter: &SiteMaintenanceReceiptFilter,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<SiteMaintenanceReceipt>, StoreError> {
        self.with_app_read(|app| app.list_site_maintenance_receipts(filter, after, limit))
    }

    pub fn list_unresolved_site_maintenance_receipts(
        &self,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<SiteMaintenanceReceipt>, StoreError> {
        self.with_app_read(|app| app.list_unresolved_site_maintenance_receipts(after, limit))
    }
}
