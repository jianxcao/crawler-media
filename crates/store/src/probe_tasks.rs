mod counters;
mod optional;

use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

#[derive(Clone, Debug)]
pub struct ProbeJob {
    pub id: String,
    pub kind: String,
    pub media_id: String,
    pub season: Option<u32>,
    pub status: String,
    pub total: usize,
    pub completed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub partial: usize,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ProbeJobUnit {
    pub job_id: String,
    pub ledger_id: String,
    pub kind: String,
    pub force_fingerprint: bool,
    pub reuse_fingerprint_cache: bool,
    pub overwrite_markers: bool,
    pub status: String,
    pub error: Option<String>,
    pub sampling_plan_json: Option<String>,
    pub detection_outcome_json: Option<String>,
    pub reuse_media_info_cache: bool,
    /// Bounded dispatch window this unit belongs to; persisted, never guessed.
    pub stage: super::ProbeDispatchStage,
}

pub struct ProbeJobUnitSpec<'a> {
    pub ledger_id: &'a str,
    pub kind: &'a str,
    pub force_fingerprint: bool,
    pub reuse_fingerprint_cache: bool,
    pub overwrite_markers: bool,
    pub reuse_media_info_cache: bool,
}

impl ProbeJob {
    pub fn is_active(&self) -> bool {
        self.status == "queued" || self.status == "running"
    }

    pub fn elapsed_ms(&self, now_ms: i64) -> u64 {
        let end = self.finished_at_ms.unwrap_or(now_ms);
        let start = self.started_at_ms.unwrap_or(self.created_at_ms);
        end.saturating_sub(start).max(0) as u64
    }
}

impl Store {
    pub fn create_probe_job(
        &self,
        id: &str,
        kind: &str,
        media_id: &str,
        season: Option<u32>,
        scope_key: &str,
        units: &[ProbeJobUnitSpec<'_>],
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| {
            db.create_probe_job(id, kind, media_id, season, scope_key, units)
        })
    }
    /// Requeue interrupted units and reconcile counters without loading work.
    ///
    /// Startup must not materialize every pending unit: the bounded dispatch
    /// windows decide what runs next, and the SQL here only repairs durable
    /// status/counters for active jobs.
    pub fn reset_running_probe_units(&self) -> Result<(), StoreError> {
        self.with_library_write(|db| db.reset_running_probe_units())
    }

    /// Active jobs of one kind with their units. Marker refreshes are rare
    /// enough to inspect whole; this is not used for ordinary backlog jobs.
    pub fn active_probe_jobs_by_kind(
        &self,
        kind: &str,
    ) -> Result<Vec<(ProbeJob, Vec<ProbeJobUnit>)>, StoreError> {
        self.with_library_read(|db| db.active_probe_jobs_by_kind(kind))
    }

    /// Active non-marker jobs whose units are all terminal.
    pub fn active_completed_probe_jobs(&self, limit: usize) -> Result<Vec<ProbeJob>, StoreError> {
        self.with_library_read(|db| db.active_completed_probe_jobs(limit))
    }

    /// A bounded window of active unit references for removed-library repair.
    pub fn active_probe_unit_refs(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String)>, StoreError> {
        self.with_library_read(|db| db.active_probe_unit_refs(limit))
    }

    pub fn recover_probe_jobs(&self) -> Result<Vec<(ProbeJob, Vec<ProbeJobUnit>)>, StoreError> {
        self.with_library_write(|db| db.recover_probe_jobs())
    }

    pub fn probe_job_units(&self, job_id: &str) -> Result<Vec<ProbeJobUnit>, StoreError> {
        self.with_library_read(|db| db.probe_job_units(job_id))
    }

    pub fn start_probe_unit(&self, job_id: &str, ledger_id: &str) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.start_probe_unit(job_id, ledger_id))
    }

    /// Requeue only the interrupted unit while its job is still active.
    pub fn retry_probe_unit(&self, job_id: &str, ledger_id: &str) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.retry_probe_unit(job_id, ledger_id))
    }

    /// Complete metadata and persist its successor as one durable transition.
    pub fn complete_media_probe_and_enqueue_fingerprint(
        &self,
        job_id: &str,
        ledger_id: &str,
        fingerprint_job_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| {
            db.complete_media_probe_and_enqueue_fingerprint(job_id, ledger_id, fingerprint_job_id)
        })
    }

    pub fn put_probe_sampling_plan(
        &self,
        job_id: &str,
        ledger_id: &str,
        sampling_plan_json: &str,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| {
            db.put_probe_sampling_plan(job_id, ledger_id, sampling_plan_json)
        })
    }

    pub fn put_probe_detection_outcome(
        &self,
        job_id: &str,
        ledger_id: &str,
        detection_outcome_json: &str,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| {
            db.put_probe_detection_outcome(job_id, ledger_id, detection_outcome_json)
        })
    }

    pub fn finish_probe_unit(
        &self,
        job_id: &str,
        ledger_id: &str,
        succeeded: bool,
        error: Option<&str>,
    ) -> Result<(ProbeJob, bool, bool), StoreError> {
        self.with_library_write(|db| db.finish_probe_unit(job_id, ledger_id, succeeded, error))
    }

    pub fn cancel_probe_unit(
        &self,
        job_id: &str,
        ledger_id: &str,
        reason: &str,
    ) -> Result<(ProbeJob, bool, bool), StoreError> {
        self.with_library_write(|db| db.cancel_probe_unit(job_id, ledger_id, reason))
    }

    /// Cancel an atomic refresh, terminalizing pending units and releasing the scope together.
    pub fn cancel_probe_job(&self, job_id: &str, reason: &str) -> Result<(), StoreError> {
        self.with_library_write(|db| db.cancel_probe_job(job_id, reason))
    }

    pub fn finish_probe_unit_with_status(
        &self,
        job_id: &str,
        ledger_id: &str,
        status: &str,
        _error_kind: Option<&str>,
        error: Option<&str>,
    ) -> Result<(ProbeJob, bool, bool), StoreError> {
        self.with_library_write(|db| {
            db.finish_probe_unit_with_status(job_id, ledger_id, status, _error_kind, error)
        })
    }

    pub fn finish_probe_unit_with_status_and_vm_steps(
        &self,
        job_id: &str,
        ledger_id: &str,
        status: &str,
        error_kind: Option<&str>,
        error: Option<&str>,
    ) -> Result<(ProbeJob, bool, bool, i64), StoreError> {
        self.with_library_write(|db| {
            db.finish_probe_unit_with_status_and_vm_steps(
                job_id, ledger_id, status, error_kind, error,
            )
        })
    }

    pub fn reconcile_probe_job_counts(&self, job_id: &str) -> Result<ProbeJob, StoreError> {
        self.with_library_write(|db| db.reconcile_probe_job_counts(job_id))
    }

    pub fn finish_probe_job(
        &self,
        job_id: &str,
        succeeded: bool,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.finish_probe_job(job_id, succeeded, error))
    }

    /// Terminalize a fully completed job when startup cannot read its refreshed state.
    /// The completion predicate prevents this recovery fallback from failing pending work.
    pub fn fail_completed_probe_job(&self, job_id: &str, error: &str) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.fail_completed_probe_job(job_id, error))
    }

    pub fn get_probe_job(&self, id: &str) -> Result<Option<ProbeJob>, StoreError> {
        self.with_library_read(|db| db.get_probe_job(id))
    }

    pub fn latest_probe_job_for_scope(
        &self,
        scope_key: &str,
    ) -> Result<Option<ProbeJob>, StoreError> {
        self.with_library_read(|db| db.latest_probe_job_for_scope(scope_key))
    }

    pub fn latest_marker_refresh_for_season(
        &self,
        media_id: &str,
        season: u32,
    ) -> Result<Option<ProbeJob>, StoreError> {
        self.with_library_read(|db| db.latest_marker_refresh_for_season(media_id, season))
    }

    pub fn active_probe_job_for_scope(
        &self,
        scope_key: &str,
    ) -> Result<Option<ProbeJob>, StoreError> {
        self.with_library_read(|db| db.active_probe_job_for_scope(scope_key))
    }

    pub fn is_probe_queued(&self, ledger_id: &str) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.is_probe_queued(ledger_id))
    }

    /// File ids that currently have a queued or running probe unit.
    ///
    /// One round-trip for the detail / files pages: callers must run this on a
    /// reader (or inside `store_read`), never by taking the writer lock per file.
    pub fn probe_queued_among(
        &self,
        file_ids: &[domain::LedgerId],
    ) -> Result<std::collections::HashSet<domain::LedgerId>, StoreError> {
        self.with_library_read(|db| db.probe_queued_among(file_ids))
    }

    /// Whether this exact unit still belongs to an active job.
    pub fn is_probe_unit_active(&self, job_id: &str, ledger_id: &str) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.is_probe_unit_active(job_id, ledger_id))
    }

    /// 这个台账行是否**曾经**被排入 probe（不区分当前状态）。
    ///
    /// `is_probe_queued` 只反映"此刻是否仍在队列里"，探测 worker 可能在调用方
    /// 观察之前就把它跑完；需要断言"扫描确实登记了探测"时用这个，避免把调度
    /// 时序当成业务结果。
    pub fn probe_unit_recorded_for_ledger(&self, ledger_id: &str) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.probe_unit_recorded_for_ledger(ledger_id))
    }

    /// Job of the most recent probe unit for this file, in any state.
    pub fn latest_probe_job_for_ledger(
        &self,
        ledger_id: &str,
    ) -> Result<Option<String>, StoreError> {
        self.with_library_read(|db| db.latest_probe_job_for_ledger(ledger_id))
    }

    pub fn active_probe_unit_for_ledger(
        &self,
        ledger_id: &str,
    ) -> Result<Option<ProbeJobUnit>, StoreError> {
        self.with_library_read(|db| db.active_probe_unit_for_ledger(ledger_id))
    }
}

fn map_probe_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProbeJob> {
    Ok(ProbeJob {
        id: row.get(0)?,
        kind: row.get(1)?,
        media_id: row.get(2)?,
        season: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
        status: row.get(4)?,
        total: row.get::<_, i64>(5)? as usize,
        completed: row.get::<_, i64>(6)? as usize,
        succeeded: row.get::<_, i64>(7)? as usize,
        failed: row.get::<_, i64>(8)? as usize,
        partial: row.get::<_, i64>(9)? as usize,
        error: row.get(10)?,
        created_at_ms: row.get(11)?,
        started_at_ms: row.get(12)?,
        finished_at_ms: row.get(13)?,
    })
}

fn is_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

/// Terminal status a probe job takes once every remaining unit is terminal.
///
/// `None` means the job must stay active. Every kind terminalizes as soon as
/// its last unit finishes, except `marker_refresh`, which only becomes
/// `succeeded` together with its marker results inside
/// `complete_marker_refresh` — unless a unit already failed.
///
/// Both the normal unit-completion path and ledger-retirement resync share this
/// decision so the two cannot disagree about when a job is done.
pub(crate) fn probe_job_terminal_status(
    kind: &str,
    total: usize,
    completed: usize,
    failed: usize,
    partial: usize,
    cancelled: usize,
) -> Option<&'static str> {
    if completed < total {
        return None;
    }
    if failed > 0 {
        return Some("failed");
    }
    match kind {
        _ if cancelled > 0 => Some("cancelled"),
        "marker_refresh" => None,
        _ if partial > 0 => Some("partial"),
        _ => Some("succeeded"),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[path = "probe_tasks/connection.rs"]
mod connection;
