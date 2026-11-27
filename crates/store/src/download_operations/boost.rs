use domain::DownloaderId;
use rusqlite::{Connection, OptionalExtension, params};

use super::boost_facts::upsert_task_fact_in_tx;
use super::types::*;
use super::{parse_opt_id, validate_non_empty};
use crate::maps::parse_id;
use crate::plugins::PluginError;
use crate::{Store, StoreError};

pub(super) const RES_COLS: &str = "reservation_id, operation_id, quota_key, plugin_id, site_id, downloader_id, info_hash, bytes_digest, reserved_bytes, stage, expires_at, revision, created_at, updated_at";
pub(super) const TF_COLS: &str = "task_fact_id, plugin_id, quota_key, downloader_id, ownership_tag, info_hash, site_id, state, occupied_bytes, retained_bytes, hr_active, hr_until, hold_until, free_until, protection_state, protection_reason, cleanup_plan_handle, cleanup_plan_expires_at, cleanup_plan_json, observed_at, revision, created_at, updated_at";

pub(super) fn map_reservation(row: &rusqlite::Row<'_>) -> rusqlite::Result<BoostReservationRecord> {
    let stage_raw: String = row.get(9)?;
    Ok(BoostReservationRecord {
        reservation_id: row.get(0)?,
        operation_id: row.get(1)?,
        quota_key: row.get(2)?,
        plugin_id: row.get(3)?,
        site_id: parse_opt_id(row.get(4)?)?,
        downloader_id: parse_id(row.get(5)?, 5)?,
        info_hash: row.get(6)?,
        bytes_digest: row.get(7)?,
        reserved_bytes: row.get(8)?,
        stage: stage_raw
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        expires_at: row.get(10)?,
        revision: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

pub(super) fn map_task_fact(row: &rusqlite::Row<'_>) -> rusqlite::Result<BoostTaskFactRecord> {
    let state_raw: String = row.get(7)?;
    let prot_raw: String = row.get(14)?;
    let hr_active_int: Option<i64> = row.get(10)?;
    Ok(BoostTaskFactRecord {
        task_fact_id: row.get(0)?,
        plugin_id: row.get(1)?,
        quota_key: row.get(2)?,
        downloader_id: parse_id(row.get(3)?, 3)?,
        ownership_tag: row.get(4)?,
        info_hash: row.get(5)?,
        site_id: parse_opt_id(row.get(6)?)?,
        state: state_raw
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        occupied_bytes: row.get(8)?,
        retained_bytes: row.get(9)?,
        hr_active: hr_active_int.map(|v| v != 0),
        hr_until: row.get(11)?,
        hold_until: row.get(12)?,
        free_until: row.get(13)?,
        protection_state: prot_raw
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        protection_reason: row.get(15)?,
        cleanup_plan_handle: row.get(16)?,
        cleanup_plan_expires_at: row.get(17)?,
        cleanup_plan_json: row.get(18)?,
        observed_at: row.get(19)?,
        revision: row.get(20)?,
        created_at: row.get(21)?,
        updated_at: row.get(22)?,
    })
}

fn fetch_res_by_op(
    conn: &Connection,
    operation_id: &str,
) -> Result<Option<BoostReservationRecord>, StoreError> {
    conn.query_row(
        &format!("SELECT {RES_COLS} FROM boost_reservations WHERE operation_id = ?1"),
        params![operation_id],
        map_reservation,
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn reconcile_reservation_in_tx(
    tx: &Connection,
    operation_id: &str,
    resolved_state: DownloadOperationState,
    task_fact: Option<&UpsertBoostTaskFactInput>,
    now_utc: i64,
) -> Result<(), StoreError> {
    let Some(res) = fetch_res_by_op(tx, operation_id)? else {
        return Ok(());
    };
    if matches!(
        res.stage,
        BoostReservationStage::Committed | BoostReservationStage::Released
    ) {
        return Ok(());
    }
    let next_stage = if resolved_state == DownloadOperationState::Submitted {
        BoostReservationStage::Committed
    } else {
        BoostReservationStage::Released
    };
    tx.execute(
        "UPDATE boost_reservations SET stage = ?2, revision = revision + 1, updated_at = ?3
         WHERE reservation_id = ?1",
        params![res.reservation_id, next_stage.as_str(), now_utc],
    )?;
    if resolved_state == DownloadOperationState::Submitted
        && let Some(tf) = task_fact
    {
        upsert_task_fact_in_tx(tx, tf)?;
    }
    Ok(())
}

fn has_active_boost_hash(
    tx: &Connection,
    quota_key: &str,
    downloader_id: DownloaderId,
    norm_hash: &str,
    exclude_operation_id: &str,
) -> Result<bool, StoreError> {
    let dl_str = downloader_id.to_string();
    let in_res: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM boost_reservations
         WHERE quota_key = ?1 AND downloader_id = ?2 AND LOWER(info_hash) = ?3
           AND stage IN ('provisional','final','unknown_held') AND operation_id != ?4)",
        params![quota_key, dl_str, norm_hash, exclude_operation_id],
        |r| r.get(0),
    )?;
    if in_res {
        return Ok(true);
    }
    let in_facts: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM boost_task_facts
         WHERE quota_key = ?1 AND downloader_id = ?2 AND LOWER(info_hash) = ?3
           AND state IN ('downloading','seeding','paused'))",
        params![quota_key, dl_str, norm_hash],
        |r| r.get(0),
    )?;
    Ok(in_facts)
}

fn check_boost_budget_and_hourly(
    tx: &Connection,
    res: &BoostReservationRecord,
    input: &CommitBoostFinalInput,
) -> Result<(), StoreError> {
    if let Some(hourly_cap) = input.max_add_per_hour {
        let adds: i64 = tx.query_row(
            "SELECT COUNT(*) FROM boost_reservations
             WHERE plugin_id = ?1
               AND stage IN ('final','unknown_held','committed')
               AND created_at > ?2",
            params![res.plugin_id, input.now_utc.saturating_sub(3600)],
            |r| r.get(0),
        )?;
        if adds >= hourly_cap {
            return Err(PluginError::QuotaExceeded.into());
        }
    }
    let (used, retained): (i64, i64) = tx.query_row(
        "SELECT COALESCE(SUM(occupied_bytes), 0), COALESCE(SUM(retained_bytes), 0)
         FROM boost_task_facts WHERE quota_key = ?1 AND state != 'cleaned'",
        params![res.quota_key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let reserved: i64 = tx.query_row(
        "SELECT COALESCE(SUM(reserved_bytes), 0)
         FROM boost_reservations
         WHERE quota_key = ?1 AND stage IN ('final', 'unknown_held')",
        params![res.quota_key],
        |r| r.get(0),
    )?;
    let projected = used
        .checked_add(reserved)
        .and_then(|v| v.checked_add(retained))
        .and_then(|v| v.checked_add(input.verified_bytes))
        .ok_or(PluginError::QuotaExceeded)?;
    if projected > input.budget_bytes {
        return Err(PluginError::QuotaExceeded.into());
    }
    Ok(())
}

impl Store {
    pub fn reserve_boost_provisional(
        &self,
        input: &ReserveBoostProvisionalInput,
    ) -> Result<BoostReservationRecord, StoreError> {
        validate_non_empty(&input.reservation_id, "reservation_id must not be empty")?;
        validate_non_empty(&input.operation_id, "operation_id must not be empty")?;
        validate_non_empty(&input.quota_key, "quota_key must not be empty")?;
        validate_non_empty(&input.plugin_id, "plugin_id must not be empty")?;
        if input.estimated_bytes < 0 {
            return Err(PluginError::Invalid("estimated_bytes must be >= 0").into());
        }
        let norm_hash = input
            .info_hash
            .as_deref()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty());
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            if let Some(ref hash) = norm_hash
                && has_active_boost_hash(
                    &tx,
                    &input.quota_key,
                    input.downloader_id,
                    hash,
                    &input.operation_id,
                )?
            {
                return Err(PluginError::Conflict.into());
            }
            tx.execute(
                "INSERT INTO boost_reservations (
                    reservation_id, operation_id, quota_key, plugin_id, site_id, downloader_id,
                    info_hash, bytes_digest, reserved_bytes, stage, expires_at,
                    revision, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, 'provisional', ?9, 1, ?10, ?10)",
                params![
                    input.reservation_id,
                    input.operation_id,
                    input.quota_key,
                    input.plugin_id,
                    input.site_id.map(|id| id.to_string()),
                    input.downloader_id.to_string(),
                    norm_hash,
                    input.estimated_bytes,
                    input.expires_at,
                    input.now_utc,
                ],
            )?;
            let created =
                fetch_res_by_op(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(created)
        })
    }

    pub fn commit_boost_final_reservation(
        &self,
        input: &CommitBoostFinalInput,
    ) -> Result<BoostReservationRecord, StoreError> {
        validate_non_empty(&input.operation_id, "operation_id must not be empty")?;
        validate_non_empty(&input.info_hash, "info_hash must not be empty")?;
        validate_non_empty(&input.bytes_digest, "bytes_digest must not be empty")?;
        if input.verified_bytes < 0 || input.budget_bytes < 0 || input.now_utc < 0 {
            return Err(PluginError::Invalid("bytes, budget, and now_utc must be >= 0").into());
        }
        let norm_hash = input.info_hash.trim().to_ascii_lowercase();
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let res = fetch_res_by_op(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            if res.stage != BoostReservationStage::Provisional
                || res.revision != input.expected_reservation_revision
            {
                return Err(PluginError::Conflict.into());
            }
            let op = super::operations::fetch_operation_by_id(&tx, &input.operation_id)?
                .ok_or(PluginError::NotFound)?;
            if !matches!(
                op.state,
                DownloadOperationState::Intent | DownloadOperationState::Reserved
            ) || (input.mark_operation_submitting
                && op
                    .downloader_id
                    .is_some_and(|existing| existing != res.downloader_id))
            {
                return Err(PluginError::Conflict.into());
            }
            if has_active_boost_hash(
                &tx,
                &res.quota_key,
                res.downloader_id,
                &norm_hash,
                &input.operation_id,
            )? {
                return Err(PluginError::Conflict.into());
            }
            check_boost_budget_and_hourly(&tx, &res, input)?;
            tx.execute(
                "UPDATE boost_reservations
                 SET stage = 'final', info_hash = ?3, bytes_digest = ?4, reserved_bytes = ?5,
                     revision = revision + 1, updated_at = ?6
                 WHERE reservation_id = ?1 AND revision = ?2",
                params![
                    res.reservation_id,
                    input.expected_reservation_revision,
                    norm_hash,
                    input.bytes_digest,
                    input.verified_bytes,
                    input.now_utc,
                ],
            )?;
            if input.mark_operation_submitting {
                let changed = tx.execute(
                    "UPDATE download_operations
                     SET state = 'submitting',
                         downloader_id = COALESCE(downloader_id, ?6),
                         info_hash = ?2, bytes_digest = ?3, verified_bytes = ?4,
                         revision = revision + 1, updated_at = ?5
                     WHERE operation_id = ?1 AND state IN ('intent', 'reserved')",
                    params![
                        input.operation_id,
                        norm_hash,
                        input.bytes_digest,
                        input.verified_bytes,
                        input.now_utc,
                        res.downloader_id.to_string(),
                    ],
                )?;
                if changed != 1 {
                    return Err(PluginError::Conflict.into());
                }
            }
            let updated =
                fetch_res_by_op(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn mark_boost_reservation_unknown_held(
        &self,
        operation_id: &str,
        expected_revision: i64,
        now_utc: i64,
    ) -> Result<BoostReservationRecord, StoreError> {
        validate_non_empty(operation_id, "operation_id must not be empty")?;
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let res = fetch_res_by_op(&tx, operation_id)?.ok_or(PluginError::NotFound)?;
            if res.stage != BoostReservationStage::Final || res.revision != expected_revision {
                return Err(PluginError::Conflict.into());
            }
            tx.execute(
                "UPDATE boost_reservations
                 SET stage = 'unknown_held', revision = revision + 1, updated_at = ?3
                 WHERE reservation_id = ?1 AND revision = ?2",
                params![res.reservation_id, expected_revision, now_utc],
            )?;
            let updated = fetch_res_by_op(&tx, operation_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn release_boost_reservation(
        &self,
        operation_id: &str,
        expected_revision: i64,
        now_utc: i64,
    ) -> Result<BoostReservationRecord, StoreError> {
        validate_non_empty(operation_id, "operation_id must not be empty")?;
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let res = fetch_res_by_op(&tx, operation_id)?.ok_or(PluginError::NotFound)?;
            if !matches!(
                res.stage,
                BoostReservationStage::Provisional
                    | BoostReservationStage::Final
                    | BoostReservationStage::UnknownHeld
            ) || res.revision != expected_revision
            {
                return Err(PluginError::Conflict.into());
            }
            tx.execute(
                "UPDATE boost_reservations
                 SET stage = 'released', revision = revision + 1, updated_at = ?3
                 WHERE reservation_id = ?1 AND revision = ?2",
                params![res.reservation_id, expected_revision, now_utc],
            )?;
            let updated = fetch_res_by_op(&tx, operation_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn finalize_boost_reservation_to_task(
        &self,
        input: &FinalizeBoostTaskInput,
    ) -> Result<(BoostReservationRecord, BoostTaskFactRecord), StoreError> {
        validate_non_empty(&input.operation_id, "operation_id must not be empty")?;
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let res = fetch_res_by_op(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            if !matches!(
                res.stage,
                BoostReservationStage::Final | BoostReservationStage::UnknownHeld
            ) || res.revision != input.expected_reservation_revision
            {
                return Err(PluginError::Conflict.into());
            }
            tx.execute(
                "UPDATE boost_reservations
                 SET stage = 'committed', revision = revision + 1, updated_at = ?3
                 WHERE reservation_id = ?1 AND revision = ?2",
                params![
                    res.reservation_id,
                    input.expected_reservation_revision,
                    input.task_fact.now_utc,
                ],
            )?;
            let tf = upsert_task_fact_in_tx(&tx, &input.task_fact)?;
            let updated_res =
                fetch_res_by_op(&tx, &input.operation_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok((updated_res, tf))
        })
    }

    pub fn get_boost_snapshot_facts(
        &self,
        quota_key: &str,
        downloader_ids: &[DownloaderId],
    ) -> Result<BoostSnapshotFacts, StoreError> {
        validate_non_empty(quota_key, "quota_key must not be empty")?;
        let filter_dls: std::collections::HashSet<DownloaderId> =
            downloader_ids.iter().copied().collect();
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut r_stmt = tx.prepare(&format!(
                "SELECT {RES_COLS} FROM boost_reservations
                 WHERE quota_key = ?1 AND stage IN ('provisional','final','unknown_held')
                 ORDER BY updated_at DESC, reservation_id ASC"
            ))?;
            let reservations = r_stmt
                .query_map(params![quota_key], map_reservation)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|r| filter_dls.is_empty() || filter_dls.contains(&r.downloader_id))
                .collect::<Vec<_>>();
            drop(r_stmt);

            let mut t_stmt = tx.prepare(&format!(
                "SELECT {TF_COLS} FROM boost_task_facts
                 WHERE quota_key = ?1 AND state != 'cleaned'
                 ORDER BY updated_at DESC, task_fact_id ASC"
            ))?;
            let tasks = t_stmt
                .query_map(params![quota_key], map_task_fact)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|t| filter_dls.is_empty() || filter_dls.contains(&t.downloader_id))
                .collect::<Vec<_>>();
            drop(t_stmt);
            tx.commit()?;

            let reserved_bytes: i64 = reservations
                .iter()
                .filter(|r| {
                    matches!(
                        r.stage,
                        BoostReservationStage::Final | BoostReservationStage::UnknownHeld
                    )
                })
                .try_fold(0i64, |acc, r| acc.checked_add(r.reserved_bytes))
                .ok_or(PluginError::QuotaExceeded)?;
            let used_bytes: i64 = tasks
                .iter()
                .try_fold(0i64, |acc, t| acc.checked_add(t.occupied_bytes))
                .ok_or(PluginError::QuotaExceeded)?;
            let retained_file_bytes: i64 = tasks
                .iter()
                .try_fold(0i64, |acc, t| acc.checked_add(t.retained_bytes))
                .ok_or(PluginError::QuotaExceeded)?;

            Ok(BoostSnapshotFacts {
                used_bytes,
                reserved_bytes,
                retained_file_bytes,
                reservations,
                tasks,
            })
        })
    }

    pub fn get_boost_reservation_by_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<BoostReservationRecord>, StoreError> {
        validate_non_empty(operation_id, "operation_id must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let row = fetch_res_by_op(&tx, operation_id)?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn count_boost_adds_since(
        &self,
        plugin_id: &str,
        since_utc: i64,
    ) -> Result<i64, StoreError> {
        validate_non_empty(plugin_id, "plugin_id must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM boost_reservations
                 WHERE plugin_id = ?1
                   AND stage IN ('final','unknown_held','committed')
                   AND created_at >= ?2",
                params![plugin_id, since_utc],
                |r| r.get(0),
            )?;
            tx.commit()?;
            Ok(count)
        })
    }
}
