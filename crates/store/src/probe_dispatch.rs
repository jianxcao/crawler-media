//! Bounded dispatch identity for durable probe work.
//!
//! `ProbeDispatchStage` classifies *which bounded worker window* a queued
//! `probe_job_units` row belongs to. It is not the same fact as the
//! `probe_stage_state` Metadata/Intro/Outro rows: those record the result of a
//! probe, while this stage only says where the pending unit is dispatched.
//!
//! The classification is persisted on the unit (`probe_job_units.stage`) and
//! derives from the job kind when the unit is created. A metadata result that
//! transitions a `marker_refresh` unit to fingerprint work writes the new stage
//! in the same database update, so a restart resumes from the durable state
//! instead of guessing from an in-memory work item.

use rusqlite::{OptionalExtension, params};

use super::{ProbeJob, ProbeJobUnit, Store, StoreError};

/// The dispatch refill statement, shared with the plan test so the assertion can never drift
/// from production. `?1` = stage, `?2` = limit.
#[doc(hidden)]
pub const PENDING_DISPATCH_SQL: &str = "SELECT u.job_id, u.file_id
     FROM probe_job_units u
     JOIN probe_jobs j ON j.id = u.job_id
     WHERE u.stage = ?1 AND u.status = 'queued'
       AND j.status IN ('queued', 'running')
     ORDER BY u.job_id
     LIMIT ?2";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProbeDispatchStage {
    Metadata,
    Fingerprint,
}

impl ProbeDispatchStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Fingerprint => "fingerprint",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "metadata" => Some(Self::Metadata),
            "fingerprint" => Some(Self::Fingerprint),
            _ => None,
        }
    }

    /// The stage a newly created unit starts in, derived from the job kind.
    pub(crate) fn for_job_kind(kind: &str) -> Self {
        if kind == "fingerprint_probe" {
            Self::Fingerprint
        } else {
            Self::Metadata
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProbeDispatchId {
    pub job_id: String,
    pub ledger_id: String,
    pub stage: ProbeDispatchStage,
}

/// One durable dispatch item loaded from a single ID.
///
/// The store loads exactly this row; it never materializes the whole pending
/// backlog. `source_version` is the metadata result's persisted source version
/// (empty when no metadata has been written yet).
#[derive(Clone, Debug)]
pub struct ProbeDispatchSnapshot {
    pub job: ProbeJob,
    pub unit: ProbeJobUnit,
    pub row: domain::FileCoverage,
    pub source_version: String,
}

impl Store {
    /// Pending dispatch IDs for one stage, capped by `limit`.
    ///
    /// # Order
    ///
    /// Job order, not arrival order. `probe_unit_dispatch(stage, status, job_id)` is partial
    /// on `status = 'queued'`, so with `stage` and `status` fixed the index already *is* the
    /// `job_id` order: the window is read with `limit` index entries and `limit` job probes,
    /// and the cost does not depend on the size of the backlog.
    ///
    /// Arrival order (`probe_jobs.created_at_ms`) has no index, so SQLite materialised and
    /// sorted the whole queued set on every refill. Measured VM steps for one 64-wide refill
    /// (2,000 / 20,000 queued units): arrival order 30,339 / 300,339, `job_id` 785 / 785.
    /// `ORDER BY u.rowid` does not help either — it still sorts (26,403 / 260,403) because
    /// no index supplies rowid order for a filtered subset, and it would break outright if
    /// the table ever became `WITHOUT ROWID`.
    ///
    /// FIFO is not load-bearing here: a dispatched unit leaves `status = 'queued'`, so any
    /// deterministic order over a shrinking set dispatches every unit (no starvation), the
    /// worst case wait stays `backlog / window` refills, and grouping one job's units
    /// together is what the worker wants anyway. Strict arrival order *is* reachable
    /// index-backed, but only with DDL (see `docs` in the task history): carry
    /// `created_at_ms` onto `probe_job_units` and index
    /// `(stage, status, created_at_ms, job_id) WHERE status = 'queued'`.
    pub fn pending_probe_dispatch_ids(
        &self,
        stage: ProbeDispatchStage,
        limit: usize,
    ) -> Result<Vec<ProbeDispatchId>, StoreError> {
        self.pending_probe_dispatch_ids_with_vm_steps(stage, limit)
            .map(|(ids, _)| ids)
    }

    /// Same read as [`Self::pending_probe_dispatch_ids`], additionally reporting the SQLite
    /// VM steps it consumed, so tests can assert the refill cost as a counter.
    #[doc(hidden)]
    pub fn pending_probe_dispatch_ids_with_vm_steps(
        &self,
        stage: ProbeDispatchStage,
        limit: usize,
    ) -> Result<(Vec<ProbeDispatchId>, i64), StoreError> {
        if limit == 0 {
            return Ok((Vec::new(), 0));
        }
        let mut statement = self.library.prepare(PENDING_DISPATCH_SQL)?;
        let rows = statement.query_map(params![stage.as_str(), limit as i64], |row| {
            Ok(ProbeDispatchId {
                job_id: row.get(0)?,
                ledger_id: row.get(1)?,
                stage,
            })
        })?;
        let ids = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(statement.get_status(rusqlite::StatementStatus::VmStep));
        Ok((ids, vm_steps))
    }

    /// Load the job, unit, ledger row, and persisted source version for one ID.
    pub fn load_probe_dispatch_snapshot(
        &self,
        id: &ProbeDispatchId,
    ) -> Result<Option<ProbeDispatchSnapshot>, StoreError> {
        let unit = self
            .library
            .query_row(
                "SELECT u.job_id, u.file_id, u.kind, u.force_fingerprint,
                        u.reuse_fingerprint_cache, u.overwrite_markers, u.status, u.error,
                        u.sampling_plan_json, u.detection_outcome_json,
                        u.reuse_media_info_cache, u.stage
                 FROM probe_job_units u
                 JOIN probe_jobs j ON j.id = u.job_id
                 WHERE u.job_id = ?1 AND u.file_id = ?2",
                params![id.job_id, id.ledger_id],
                |row| {
                    Ok(ProbeJobUnit {
                        job_id: row.get(0)?,
                        ledger_id: row.get(1)?,
                        kind: row.get(2)?,
                        force_fingerprint: row.get(3)?,
                        reuse_fingerprint_cache: row.get(4)?,
                        overwrite_markers: row.get(5)?,
                        status: row.get(6)?,
                        error: row.get(7)?,
                        sampling_plan_json: row.get(8)?,
                        detection_outcome_json: row.get(9)?,
                        reuse_media_info_cache: row.get::<_, i64>(10).unwrap_or(0) != 0,
                        stage: row
                            .get::<_, String>(11)
                            .ok()
                            .and_then(|value| ProbeDispatchStage::from_str(&value))
                            .unwrap_or(ProbeDispatchStage::Metadata),
                    })
                },
            )
            .optional()?;
        let Some(unit) = unit else {
            return Ok(None);
        };
        let Some(job) = self.get_probe_job(&id.job_id)? else {
            return Ok(None);
        };
        // `ProbeDispatchId` carries the raw ledger id as text；读路径只接受强类型，
        // 转换失败与「该行不存在」在行为上等价（都是 `None`）。
        let Some(file_id) = id.ledger_id.parse::<domain::LedgerId>().ok() else {
            return Ok(None);
        };
        let Some(row) = self.file_coverage_by_id(file_id)? else {
            return Ok(None);
        };
        let source_version = self
            .library
            .query_row(
                "SELECT source_version FROM file_details WHERE file_id = ?1",
                params![id.ledger_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .unwrap_or_default();
        Ok(Some(ProbeDispatchSnapshot {
            job,
            unit,
            row,
            source_version,
        }))
    }

    /// Persist a completed metadata result as fingerprint work on the same unit.
    ///
    /// The unit returns to `queued` so the bounded fingerprint window can claim
    /// it; a crash between the metadata result and this transition leaves the
    /// unit running, and startup recovery requeues it at the metadata stage.
    pub fn advance_probe_unit_to_fingerprint(
        &self,
        job_id: &str,
        ledger_id: &str,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE probe_job_units SET status = 'queued', stage = 'fingerprint'
             WHERE job_id = ?1 AND file_id = ?2
               AND status IN ('running', 'queued') AND stage = 'metadata'",
            params![job_id, ledger_id],
        )?;
        Ok(changed > 0)
    }
}
