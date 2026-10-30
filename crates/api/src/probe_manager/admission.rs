//! Probe policy assessment and durable admission.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{ProbeManager, ProbeUnit, policy, probe, single_season_opt};

impl ProbeManager {
    pub fn request_probe(
        &self,
        row: &domain::FileCoverage,
        origin: policy::ProbeRequestOrigin,
    ) -> Result<policy::ProbeRequestResult, store::StoreError> {
        let now_ms = self.clock.lock().now_ms();
        self.assessments.fetch_add(1, Ordering::SeqCst);
        let source_version = crate::fingerprint_job::current_source_version(&row.file.path);
        let need = {
            let store = self.store.as_ref();
            policy::assess_probe_need(&store, &row.file, origin, now_ms, source_version)
        };
        if !matches!(need.retry, policy::ProbeRequestResult::Complete) {
            return Ok(need.retry);
        }
        if need.metadata_valid && !need.intro_missing && !need.outro_missing {
            return Ok(policy::ProbeRequestResult::Complete);
        }
        let unit = ProbeUnit {
            row: row.clone(),
            kind: need.kind,
            force_fingerprint: need.fingerprint_enabled,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        };
        if need.metadata_valid
            && need.fingerprint_enabled
            && (need.intro_missing || need.outro_missing)
        {
            return Ok(
                match self.try_enqueue_direct_fingerprint(
                    unit,
                    need.media_duration_ms,
                    need.source_version,
                    need.sample_duration_secs,
                )? {
                    Some(job_id) => policy::ProbeRequestResult::Queued { job_id },
                    None => policy::ProbeRequestResult::AlreadyRunning,
                },
            );
        }
        let ledger_id = unit.row.file.file_id.to_string();
        if let Some(next_retry_at_ms) = self.failure_backoff_until_ms(&ledger_id, now_ms) {
            return Ok(policy::ProbeRequestResult::Waiting { next_retry_at_ms });
        }
        Ok(match self.try_enqueue(unit)? {
            Some(job_id) => policy::ProbeRequestResult::Queued { job_id },
            None => policy::ProbeRequestResult::AlreadyRunning,
        })
    }

    pub fn enqueue_direct_fingerprint(
        &self,
        unit: ProbeUnit,
        media_duration_ms: Option<i64>,
        source_version: String,
        duration_secs: u32,
    ) -> Option<String> {
        match self.try_enqueue_direct_fingerprint(
            unit,
            media_duration_ms,
            source_version,
            duration_secs,
        ) {
            Ok(job) => job,
            Err(error) => {
                tracing::warn!(%error, "直接声纹请求暂不可入队，保留探测义务等待重试");
                None
            }
        }
    }

    fn try_enqueue_direct_fingerprint(
        &self,
        mut unit: ProbeUnit,
        media_duration_ms: Option<i64>,
        source_version: String,
        duration_secs: u32,
    ) -> Result<Option<String>, store::StoreError> {
        let ledger_id = unit.row.file.file_id.to_string();
        // Register the in-memory deduplication id, then release `seen` before the Store lock:
        // admission is the hot path, and a held `seen` block would serialize every worker
        // that finishes a unit behind this one Store write.
        if !self.seen.lock().insert(ledger_id.clone()) {
            return Ok(None);
        }
        let job_id = match self.persist_single_unit(&unit, "fingerprint_probe") {
            Ok(Some(job_id)) => job_id,
            Ok(None) => {
                self.seen.lock().remove(&ledger_id);
                return Ok(None);
            }
            Err(error) => {
                self.seen.lock().remove(&ledger_id);
                return Err(error);
            }
        };
        unit.job_id = Some(job_id.clone());
        let work = probe::FingerprintWork {
            unit: unit.clone(),
            media_duration_ms,
            source_version,
            duration_secs,
            start_job: true,
        };
        let id = store::ProbeDispatchId {
            job_id: job_id.clone(),
            ledger_id: ledger_id.clone(),
            stage: store::ProbeDispatchStage::Fingerprint,
        };
        if self.fingerprint_tx.try_send((id, work)).is_err() {
            self.seen.lock().remove(&ledger_id);
            tracing::info!(job_id, "声纹窗口已满，直接声纹任务保持数据库排队");
            self.wake.notify_waiters();
        }
        Ok(Some(job_id))
    }

    /// 入队探测（幂等：已在队列/执行中的跳过）。
    ///
    /// 窗口满只限制内存派发：单元已经持久化，保持数据库排队，等窗口空出后由
    /// [`Self::fill_dispatch_windows`] 重新投递。取消单元会把 durable 积压变成永久丢失。
    pub fn enqueue(&self, unit: ProbeUnit) -> bool {
        match self.try_enqueue(unit) {
            Ok(job) => job.is_some(),
            Err(error) => {
                tracing::warn!(%error, "媒体信息请求暂不可入队，保留探测义务等待重试");
                false
            }
        }
    }

    fn try_enqueue(&self, mut unit: ProbeUnit) -> Result<Option<String>, store::StoreError> {
        unit.reuse_fingerprint_cache = true;
        unit.reuse_media_info_cache = true;
        let ledger_id = unit.row.file.file_id.to_string();
        if self.seen.lock().contains(&ledger_id) {
            return Ok(None);
        }
        if self
            .last_failure
            .lock()
            .get(&ledger_id)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
        {
            return Ok(None);
        }
        self.last_failure.lock().remove(&ledger_id);
        let Some(job_id) = self.persist_single_unit(&unit, "media_probe")? else {
            return Ok(None);
        };
        unit.job_id = Some(job_id.clone());
        tracing::info!(
            path = %unit.row.file.path.display(),
            job_id,
            "【媒体信息】已进入高优先级探测队列；声纹将在媒体信息写入后另行排队"
        );
        self.seen.lock().insert(ledger_id.clone());
        if self.queue_metadata(unit).is_err() {
            self.seen.lock().remove(&ledger_id);
            tracing::info!(ledger_id, job_id, "媒体信息窗口已满，单元保持数据库排队");
            self.wake.notify_waiters();
        }
        Ok(Some(job_id))
    }

    /// 强制探测：同一文件已有任务时拒绝重复排队；终态后用户可重试。
    pub fn enqueue_force(&self, mut unit: ProbeUnit) -> bool {
        unit.reuse_fingerprint_cache = false;
        unit.reuse_media_info_cache = false;
        let ledger_id = unit.row.file.file_id.to_string();
        self.last_failure.lock().remove(&ledger_id);
        // Same hot-path rule as `enqueue`: never hold `seen` across the Store write.
        if !self.seen.lock().insert(ledger_id.clone()) {
            tracing::info!(ledger_id, "已有媒体探测任务，跳过重复强制探测");
            return false;
        }
        let job_id = match self.persist_single_unit(&unit, "media_probe") {
            Ok(Some(job_id)) => job_id,
            Ok(None) => {
                self.seen.lock().remove(&ledger_id);
                return false;
            }
            Err(error) => {
                self.seen.lock().remove(&ledger_id);
                tracing::warn!(%error, ledger_id, "强制探测未入队，等待后续请求重试");
                return false;
            }
        };
        unit.job_id = Some(job_id.clone());
        if self.queue_metadata(unit).is_err() {
            self.seen.lock().remove(&ledger_id);
            tracing::info!(ledger_id, job_id, "强制探测窗口已满，任务保持数据库排队");
            self.wake.notify_waiters();
        }
        true
    }

    fn persist_single_unit(
        &self,
        unit: &ProbeUnit,
        kind: &str,
    ) -> Result<Option<String>, store::StoreError> {
        let job_id = uuid::Uuid::new_v4().to_string();
        let ledger_id = unit.row.file.file_id.to_string();
        let scope = format!("ledger:{ledger_id}");
        let spec = crate::store::ProbeJobUnitSpec {
            ledger_id: &ledger_id,
            kind: unit.kind.as_str(),
            force_fingerprint: unit.force_fingerprint,
            reuse_fingerprint_cache: unit.reuse_fingerprint_cache,
            overwrite_markers: unit.overwrite_markers,
            reuse_media_info_cache: unit.reuse_media_info_cache,
        };
        // Presence may have changed after policy assessment. Keep this final check and
        // persistence under the same Store lock that presence writes take: a source
        // recorded missing must never gain probe work, whichever admission path asked.
        self.store.library_write(|store| {
            if store.ledger_is_missing(&ledger_id)? {
                tracing::info!(ledger_id, "源文件已标记缺失，拒绝创建探测任务");
                return Ok(None);
            }
            store
                .create_probe_job(
                    &job_id,
                    kind,
                    &unit.row.file.media_id.to_string(),
                    single_season_opt(&unit.row),
                    &scope,
                    &[spec],
                )
                .map(|created| created.then_some(job_id))
        })
    }
}
