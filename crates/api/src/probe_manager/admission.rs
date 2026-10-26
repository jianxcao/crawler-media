//! Persist probe admission before offering work to bounded dispatch windows.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{ProbeManager, ProbeUnit, policy, probe};

impl ProbeManager {
    pub fn request_probe(
        &self,
        row: &domain::LedgerRow,
        origin: policy::ProbeRequestOrigin,
    ) -> Result<policy::ProbeRequestResult, store::StoreError> {
        let now_ms = self.clock.lock().now_ms();
        self.assessments.fetch_add(1, Ordering::SeqCst);
        let source_version =
            crate::fingerprint_job::current_source_version(std::path::Path::new(&row.path));
        let need = {
            let store = self.store.lock();
            policy::assess_probe_need(&store, row, origin, now_ms, source_version)
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
                match self.enqueue_direct_fingerprint(
                    unit,
                    need.media_duration_ms,
                    need.source_version,
                    need.sample_duration_secs,
                ) {
                    Some(job_id) => policy::ProbeRequestResult::Queued { job_id },
                    None => policy::ProbeRequestResult::AlreadyRunning,
                },
            );
        }
        Ok(match self.enqueue_metadata(unit) {
            Some(job_id) => policy::ProbeRequestResult::Queued { job_id },
            None => policy::ProbeRequestResult::AlreadyRunning,
        })
    }

    pub fn enqueue_direct_fingerprint(
        &self,
        mut unit: ProbeUnit,
        media_duration_ms: Option<i64>,
        source_version: String,
        duration_secs: u32,
    ) -> Option<String> {
        let ledger_id = unit.row.id.to_string();
        let mut seen = self.seen.lock();
        if !seen.insert(ledger_id.clone()) {
            return None;
        }
        let Some(job_id) = self.persist_single_unit(&unit, "fingerprint_probe") else {
            seen.remove(&ledger_id);
            return None;
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
            ledger_id,
            stage: store::ProbeDispatchStage::Fingerprint,
        };
        if self.fingerprint_tx.try_send((id, work)).is_err() {
            seen.remove(&unit.row.id.to_string());
            tracing::info!(job_id, "声纹窗口已满，直接声纹任务保持数据库排队");
            self.wake.notify_waiters();
        }
        Some(job_id)
    }

    /// 入队探测（幂等：已在队列/执行中的跳过）。
    pub fn enqueue(&self, unit: ProbeUnit) -> bool {
        self.enqueue_metadata(unit).is_some()
    }

    fn enqueue_metadata(&self, mut unit: ProbeUnit) -> Option<String> {
        unit.reuse_fingerprint_cache = true;
        unit.reuse_media_info_cache = true;
        let ledger_id = unit.row.id.to_string();
        let mut seen = self.seen.lock();
        if seen.contains(&ledger_id) {
            return None;
        }
        if self
            .last_failure
            .lock()
            .get(&ledger_id)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
        {
            return None;
        }
        self.last_failure.lock().remove(&ledger_id);
        let job_id = self.persist_single_unit(&unit, "media_probe")?;
        unit.job_id = Some(job_id.clone());
        tracing::info!(
            path = %unit.row.path,
            job_id,
            "【媒体信息】已进入高优先级探测队列；声纹将在媒体信息写入后另行排队"
        );
        // Register before sending so a fast worker cannot finish before this
        // admission adds its ID, leaving a stale in-memory deduplication entry.
        seen.insert(ledger_id.clone());
        if self.queue_metadata(unit).is_err() {
            seen.remove(&ledger_id);
            tracing::info!(ledger_id, job_id, "媒体信息窗口已满，任务保持数据库排队");
            self.wake.notify_waiters();
        }
        Some(job_id)
    }

    /// 强制探测：同一文件已有任务时拒绝重复排队；终态后用户可重试。
    pub fn enqueue_force(&self, mut unit: ProbeUnit) -> bool {
        unit.reuse_fingerprint_cache = false;
        unit.reuse_media_info_cache = false;
        let ledger_id = unit.row.id.to_string();
        let mut seen = self.seen.lock();
        self.last_failure.lock().remove(&ledger_id);
        if !seen.insert(ledger_id.clone()) {
            tracing::info!(ledger_id, "已有媒体探测任务，跳过重复强制探测");
            return false;
        }
        let Some(job_id) = self.persist_single_unit(&unit, "media_probe") else {
            seen.remove(&ledger_id);
            return false;
        };
        unit.job_id = Some(job_id.clone());
        if self.queue_metadata(unit).is_err() {
            seen.remove(&ledger_id);
            tracing::info!(ledger_id, job_id, "强制探测窗口已满，任务保持数据库排队");
            self.wake.notify_waiters();
        }
        true
    }

    pub(super) fn persist_single_unit(&self, unit: &ProbeUnit, kind: &str) -> Option<String> {
        let job_id = uuid::Uuid::new_v4().to_string();
        let ledger_id = unit.row.id.to_string();
        let scope = format!("ledger:{ledger_id}");
        let spec = crate::store::ProbeJobUnitSpec {
            ledger_id: &ledger_id,
            kind: unit.kind.as_str(),
            force_fingerprint: unit.force_fingerprint,
            reuse_fingerprint_cache: unit.reuse_fingerprint_cache,
            overwrite_markers: unit.overwrite_markers,
            reuse_media_info_cache: unit.reuse_media_info_cache,
        };
        // Presence may have changed after policy assessment. Keep this final
        // check and persistence under the same Store lock used by presence.
        let store = self.store.lock();
        match store.ledger_is_missing(&ledger_id) {
            Ok(false) => {}
            Ok(true) => {
                tracing::info!(ledger_id, "源文件已标记缺失，拒绝创建探测任务");
                return None;
            }
            Err(error) => {
                tracing::error!(%error, ledger_id, "创建探测任务前检查源文件缺失状态失败");
                return None;
            }
        }
        match store.create_probe_job(
            &job_id,
            kind,
            &unit.row.media_id.to_string(),
            unit.row.season,
            &scope,
            &[spec],
        ) {
            Ok(true) => Some(job_id),
            Ok(false) => None,
            Err(error) => {
                tracing::error!(%error, ledger_id, "无法持久化探测任务");
                None
            }
        }
    }
}
