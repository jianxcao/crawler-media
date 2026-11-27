use domain::DownloaderId;
use rusqlite::{Connection, OptionalExtension, params};

use super::boost::{TF_COLS, map_task_fact};
use super::types::*;
use super::validate_non_empty;
use crate::plugins::PluginError;
use crate::{Store, StoreError};

pub(super) fn fetch_tf_by_id(
    conn: &Connection,
    task_fact_id: &str,
) -> Result<Option<BoostTaskFactRecord>, StoreError> {
    conn.query_row(
        &format!("SELECT {TF_COLS} FROM boost_task_facts WHERE task_fact_id = ?1"),
        params![task_fact_id],
        map_task_fact,
    )
    .optional()
    .map_err(Into::into)
}

fn fetch_tf_by_plan(
    conn: &Connection,
    cleanup_plan_handle: &str,
) -> Result<Option<BoostTaskFactRecord>, StoreError> {
    conn.query_row(
        &format!("SELECT {TF_COLS} FROM boost_task_facts WHERE cleanup_plan_handle = ?1"),
        params![cleanup_plan_handle],
        map_task_fact,
    )
    .optional()
    .map_err(Into::into)
}

fn fetch_tf_by_key(
    conn: &Connection,
    quota_key: &str,
    downloader_id: DownloaderId,
    norm_hash: &str,
) -> Result<Option<BoostTaskFactRecord>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {TF_COLS} FROM boost_task_facts
             WHERE quota_key = ?1 AND downloader_id = ?2 AND LOWER(info_hash) = ?3"
        ),
        params![quota_key, downloader_id.to_string(), norm_hash],
        map_task_fact,
    )
    .optional()
    .map_err(Into::into)
}

fn validate_task_fact_input(input: &UpsertBoostTaskFactInput) -> Result<(), StoreError> {
    validate_non_empty(&input.task_fact_id, "task_fact_id must not be empty")?;
    validate_non_empty(&input.plugin_id, "plugin_id must not be empty")?;
    validate_non_empty(&input.quota_key, "quota_key must not be empty")?;
    validate_non_empty(&input.ownership_tag, "ownership_tag must not be empty")?;
    validate_non_empty(&input.info_hash, "info_hash must not be empty")?;
    if input.occupied_bytes < 0 || input.retained_bytes < 0 {
        return Err(PluginError::Invalid("task bytes must be >= 0").into());
    }
    Ok(())
}

fn update_task_fact_by_revision(
    tx: &Connection,
    input: &UpsertBoostTaskFactInput,
    expected_rev: i64,
) -> Result<BoostTaskFactRecord, StoreError> {
    let changed = tx.execute(
        "UPDATE boost_task_facts
         SET state = ?3, occupied_bytes = ?4, retained_bytes = ?5,
             hr_active = ?6, hr_until = ?7, hold_until = ?8, free_until = ?9,
             protection_state = ?10, protection_reason = ?11,
             observed_at = ?12, revision = revision + 1, updated_at = ?13
         WHERE task_fact_id = ?1 AND revision = ?2",
        params![
            input.task_fact_id,
            expected_rev,
            input.state.as_str(),
            input.occupied_bytes,
            input.retained_bytes,
            input.hr_active.map(|b| if b { 1i64 } else { 0i64 }),
            input.hr_until,
            input.hold_until,
            input.free_until,
            input.protection_state.as_str(),
            input.protection_reason,
            input.observed_at,
            input.now_utc,
        ],
    )?;
    if changed == 0 {
        let exists = fetch_tf_by_id(tx, &input.task_fact_id)?.is_some();
        return Err(if exists {
            PluginError::Conflict
        } else {
            PluginError::NotFound
        }
        .into());
    }
    fetch_tf_by_id(tx, &input.task_fact_id)?.ok_or_else(|| PluginError::NotFound.into())
}

fn upsert_task_fact_by_key(
    tx: &Connection,
    input: &UpsertBoostTaskFactInput,
    norm_hash: &str,
) -> Result<BoostTaskFactRecord, StoreError> {
    match fetch_tf_by_key(tx, &input.quota_key, input.downloader_id, norm_hash)? {
        None => {
            tx.execute(
                "INSERT INTO boost_task_facts (
                    task_fact_id, plugin_id, quota_key, downloader_id, ownership_tag, info_hash, site_id,
                    state, occupied_bytes, retained_bytes, hr_active, hr_until, hold_until, free_until,
                    protection_state, protection_reason, observed_at, revision, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, 1, ?18, ?18)",
                params![
                    input.task_fact_id,
                    input.plugin_id,
                    input.quota_key,
                    input.downloader_id.to_string(),
                    input.ownership_tag,
                    norm_hash,
                    input.site_id.map(|id| id.to_string()),
                    input.state.as_str(),
                    input.occupied_bytes,
                    input.retained_bytes,
                    input.hr_active.map(|b| if b { 1i64 } else { 0i64 }),
                    input.hr_until,
                    input.hold_until,
                    input.free_until,
                    input.protection_state.as_str(),
                    input.protection_reason,
                    input.observed_at,
                    input.now_utc,
                ],
            )?;
            fetch_tf_by_id(tx, &input.task_fact_id)?.ok_or_else(|| PluginError::NotFound.into())
        }
        Some(existing)
            if matches!(
                existing.state,
                BoostTaskState::Cleaned
                    | BoostTaskState::RetainedOnly
                    | BoostTaskState::CleanupPlanned
            ) =>
        {
            tx.execute(
                "UPDATE boost_task_facts
                 SET task_fact_id = ?2, plugin_id = ?3, ownership_tag = ?4, info_hash = ?5,
                     site_id = ?6, state = ?7, occupied_bytes = ?8, retained_bytes = ?9,
                     hr_active = ?10, hr_until = ?11, hold_until = ?12, free_until = ?13,
                     protection_state = ?14, protection_reason = ?15,
                     cleanup_plan_handle = NULL, cleanup_plan_expires_at = NULL, cleanup_plan_json = NULL,
                     observed_at = ?16, revision = revision + 1, updated_at = ?17
                 WHERE task_fact_id = ?1",
                params![
                    existing.task_fact_id,
                    input.task_fact_id,
                    input.plugin_id,
                    input.ownership_tag,
                    norm_hash,
                    input.site_id.map(|id| id.to_string()),
                    input.state.as_str(),
                    input.occupied_bytes,
                    input.retained_bytes,
                    input.hr_active.map(|b| if b { 1i64 } else { 0i64 }),
                    input.hr_until,
                    input.hold_until,
                    input.free_until,
                    input.protection_state.as_str(),
                    input.protection_reason,
                    input.observed_at,
                    input.now_utc,
                ],
            )?;
            fetch_tf_by_id(tx, &input.task_fact_id)?.ok_or_else(|| PluginError::NotFound.into())
        }
        Some(existing) => {
            tx.execute(
                "UPDATE boost_task_facts
                 SET observed_at = ?2, revision = revision + 1, updated_at = ?3
                 WHERE task_fact_id = ?1",
                params![existing.task_fact_id, input.observed_at, input.now_utc],
            )?;
            fetch_tf_by_id(tx, &existing.task_fact_id)?.ok_or_else(|| PluginError::NotFound.into())
        }
    }
}

pub(super) fn upsert_task_fact_in_tx(
    tx: &Connection,
    input: &UpsertBoostTaskFactInput,
) -> Result<BoostTaskFactRecord, StoreError> {
    validate_task_fact_input(input)?;
    let norm_hash = input.info_hash.trim().to_ascii_lowercase();
    if norm_hash.is_empty() {
        return Err(PluginError::Invalid("info_hash must not be empty").into());
    }
    if let Some(expected_rev) = input.expected_revision {
        update_task_fact_by_revision(tx, input, expected_rev)
    } else {
        upsert_task_fact_by_key(tx, input, &norm_hash)
    }
}

impl Store {
    pub fn upsert_boost_task_fact(
        &self,
        input: &UpsertBoostTaskFactInput,
    ) -> Result<BoostTaskFactRecord, StoreError> {
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let tf = upsert_task_fact_in_tx(&tx, input)?;
            tx.commit()?;
            Ok(tf)
        })
    }

    pub fn mark_boost_task_retained(
        &self,
        task_fact_id: &str,
        expected_revision: i64,
        retained_bytes: i64,
        now_utc: i64,
    ) -> Result<BoostTaskFactRecord, StoreError> {
        self.mark_boost_task_retained_with_paths(
            task_fact_id,
            expected_revision,
            retained_bytes,
            None,
            now_utc,
        )
    }

    pub fn mark_boost_task_retained_with_paths(
        &self,
        task_fact_id: &str,
        expected_revision: i64,
        retained_bytes: i64,
        retained_paths_json: Option<&str>,
        now_utc: i64,
    ) -> Result<BoostTaskFactRecord, StoreError> {
        validate_non_empty(task_fact_id, "task_fact_id must not be empty")?;
        if retained_bytes < 0 {
            return Err(PluginError::Invalid("retained_bytes must be >= 0").into());
        }
        if let Some(raw_json) = retained_paths_json {
            serde_json::from_str::<serde_json::Value>(raw_json)
                .map_err(|_| PluginError::Invalid("retained_paths_json must be valid JSON"))?;
        }
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let existing = fetch_tf_by_id(&tx, task_fact_id)?.ok_or(PluginError::NotFound)?;
            if existing.revision != expected_revision
                || !matches!(
                    existing.state,
                    BoostTaskState::Downloading | BoostTaskState::Seeding | BoostTaskState::Paused
                )
            {
                return Err(PluginError::Conflict.into());
            }
            tx.execute(
                "UPDATE boost_task_facts
                 SET state = 'retained_only', occupied_bytes = 0, retained_bytes = ?3,
                     cleanup_plan_json = COALESCE(?4, cleanup_plan_json),
                     revision = revision + 1, updated_at = ?5
                 WHERE task_fact_id = ?1 AND revision = ?2",
                params![
                    task_fact_id,
                    expected_revision,
                    retained_bytes,
                    retained_paths_json,
                    now_utc
                ],
            )?;
            tx.execute(
                "UPDATE download_ownership
                 SET ownership_state = 'deleted', revision = revision + 1, updated_at = ?3
                 WHERE downloader_id = ?1 AND ownership_tag = ?2",
                params![
                    existing.downloader_id.to_string(),
                    existing.ownership_tag,
                    now_utc
                ],
            )?;
            let updated = fetch_tf_by_id(&tx, task_fact_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn prepare_boost_cleanup_plan(
        &self,
        input: &PrepareBoostCleanupPlanInput,
    ) -> Result<BoostTaskFactRecord, StoreError> {
        validate_non_empty(&input.task_fact_id, "task_fact_id must not be empty")?;
        validate_non_empty(
            &input.cleanup_plan_handle,
            "cleanup_plan_handle must not be empty",
        )?;
        validate_non_empty(
            &input.cleanup_plan_json,
            "cleanup_plan_json must not be empty",
        )?;
        if input.ttl_secs <= 0 || input.ttl_secs > 600 {
            return Err(PluginError::Invalid("cleanup plan ttl_secs must be in 1..=600").into());
        }
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let existing =
                fetch_tf_by_id(&tx, &input.task_fact_id)?.ok_or(PluginError::NotFound)?;
            if existing.revision != input.expected_revision {
                return Err(PluginError::Conflict.into());
            }
            if existing.state == BoostTaskState::CleanupPlanned {
                if existing
                    .cleanup_plan_expires_at
                    .is_none_or(|exp| exp >= input.now_utc)
                {
                    return Err(PluginError::Conflict.into());
                }
            } else if existing.state != BoostTaskState::RetainedOnly {
                return Err(PluginError::Denied.into());
            }
            if existing.hr_active != Some(false)
                || existing.protection_state != BoostProtectionState::Eligible
                || existing.hold_until.is_some_and(|h| h > input.now_utc)
                || existing.hr_until.is_some_and(|h| h > input.now_utc)
            {
                return Err(PluginError::Denied.into());
            }
            let expires_at = input
                .now_utc
                .checked_add(input.ttl_secs)
                .ok_or(PluginError::Invalid("cleanup expiry overflow"))?;
            tx.execute(
                "UPDATE boost_task_facts
                 SET state = 'cleanup_planned', cleanup_plan_handle = ?3,
                     cleanup_plan_expires_at = ?4, cleanup_plan_json = ?5,
                     revision = revision + 1, updated_at = ?6
                 WHERE task_fact_id = ?1 AND revision = ?2",
                params![
                    input.task_fact_id,
                    input.expected_revision,
                    input.cleanup_plan_handle,
                    expires_at,
                    input.cleanup_plan_json,
                    input.now_utc,
                ],
            )?;
            let updated = fetch_tf_by_id(&tx, &input.task_fact_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn complete_boost_cleanup(
        &self,
        cleanup_plan_handle: &str,
        expected_revision: i64,
        now_utc: i64,
    ) -> Result<BoostTaskFactRecord, StoreError> {
        validate_non_empty(cleanup_plan_handle, "cleanup_plan_handle must not be empty")?;
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let existing =
                fetch_tf_by_plan(&tx, cleanup_plan_handle)?.ok_or(PluginError::NotFound)?;
            if existing.revision != expected_revision
                || existing.state != BoostTaskState::CleanupPlanned
                || existing
                    .cleanup_plan_expires_at
                    .is_none_or(|exp| exp < now_utc)
            {
                return Err(PluginError::Conflict.into());
            }
            tx.execute(
                "UPDATE boost_task_facts
                 SET state = 'cleaned', occupied_bytes = 0, retained_bytes = 0,
                     revision = revision + 1, updated_at = ?3
                 WHERE task_fact_id = ?1 AND revision = ?2",
                params![existing.task_fact_id, expected_revision, now_utc],
            )?;
            let updated =
                fetch_tf_by_id(&tx, &existing.task_fact_id)?.ok_or(PluginError::NotFound)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    pub fn get_boost_task_fact(
        &self,
        task_fact_id: &str,
    ) -> Result<Option<BoostTaskFactRecord>, StoreError> {
        validate_non_empty(task_fact_id, "task_fact_id must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let row = fetch_tf_by_id(&tx, task_fact_id)?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn get_boost_task_fact_by_plan(
        &self,
        cleanup_plan_handle: &str,
    ) -> Result<Option<BoostTaskFactRecord>, StoreError> {
        validate_non_empty(cleanup_plan_handle, "cleanup_plan_handle must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let row = fetch_tf_by_plan(&tx, cleanup_plan_handle)?;
            tx.commit()?;
            Ok(row)
        })
    }
}
