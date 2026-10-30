//! Durable probe completion and cancellation.

#[cfg(test)]
use super::synthetic_coverage;
use super::{ProbeManager, ProbeUnit, marker_jobs, now_ms, single_episode, timings};
use domain::MediaId;

impl ProbeManager {
    pub fn cancel_ledger_work(
        &self,
        ledger_ids: &[String],
        reason: &str,
    ) -> Result<(), store::StoreError> {
        for ledger_id in ledger_ids {
            let unit = self.store.active_probe_unit_for_ledger(ledger_id)?;
            if let Some(unit) = unit {
                self.store.library_write(|store| {
                    let job = store.get_probe_job(&unit.job_id)?;
                    if job.as_ref().is_some_and(|job| job.kind == "marker_refresh") {
                        store.cancel_probe_job(&unit.job_id, reason)?;
                    } else {
                        store.cancel_probe_unit(&unit.job_id, ledger_id, reason)?;
                    }
                    Ok(())
                })?;
                let units = self.store.probe_job_units(&unit.job_id)?;
                let mut seen = self.seen.lock();
                for unit in units {
                    seen.remove(&unit.ledger_id);
                }
            }
        }
        Ok(())
    }

    /// 是否已在排队/执行（前端「探测中」状态用）。
    ///
    /// Uses the shared reader pool. Async callers use their existing off-executor
    /// read seam so reader acquisition cannot park a Tokio worker.
    pub fn is_queued(&self, ledger_id: &str) -> bool {
        let queued = self.store.is_probe_queued(ledger_id);
        match queued {
            Ok(queued) => queued,
            Err(_) => self.seen.lock().contains(ledger_id),
        }
    }

    /// True only while this ledger sits in the in-memory dispatch window.
    pub fn holds_dispatch_id(&self, ledger_id: &str) -> bool {
        self.seen.lock().contains(ledger_id)
    }

    pub(crate) fn marker_refresh_job(
        &self,
        media_id: MediaId,
        season: u32,
    ) -> Result<Option<crate::store::ProbeJob>, crate::store::StoreError> {
        self.store
            .latest_marker_refresh_for_season(&media_id.to_string(), season)
    }

    #[cfg(test)]
    pub(crate) fn take_queued_for_test(&self) -> Option<ProbeUnit> {
        let id = self.metadata_rx.lock().as_mut()?.try_recv().ok()?;
        self.seen.lock().remove(&id.ledger_id);

        if let Some((unit, _)) = self.load_dispatch_unit_inner(&id, false) {
            return Some(unit);
        }
        Some(ProbeUnit {
            row: synthetic_coverage(&id.ledger_id),
            kind: domain::MediaKind::Movie,
            force_fingerprint: false,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: Some(id.job_id),
        })
    }

    #[cfg(test)]
    pub(crate) fn take_fingerprint_for_test(&self) -> Option<ProbeUnit> {
        let (id, _work) = self.fingerprint_rx.lock().as_mut()?.try_recv().ok()?;
        if let Some((unit, _)) = self.load_dispatch_unit_inner(&id, false) {
            return Some(unit);
        }
        Some(ProbeUnit {
            row: synthetic_coverage(&id.ledger_id),
            kind: domain::MediaKind::Movie,
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: Some(id.job_id),
        })
    }

    #[cfg(test)]
    pub(crate) fn has_fingerprint_queued_for_test(&self) -> bool {
        self.fingerprint_rx
            .lock()
            .as_mut()
            .and_then(|rx| rx.try_recv().ok().map(|(id, _)| id))
            .is_some()
    }

    pub(super) fn finish(&self, unit: &ProbeUnit, succeeded: bool) {
        let status = if succeeded { "succeeded" } else { "failed" };
        self.finish_with_status(
            unit,
            status,
            None,
            (!succeeded).then_some("探测单元执行失败"),
        );
    }

    pub(crate) fn finish_with_status(
        &self,
        unit: &ProbeUnit,
        status: &str,
        error_kind: Option<&str>,
        error: Option<&str>,
    ) {
        let ledger_id = unit.row.file.file_id.to_string();
        let cancelled = unit
            .job_id
            .as_deref()
            .and_then(|id| self.store.get_probe_job(id).ok().flatten())
            .is_some_and(|job| job.status == "cancelled");
        if cancelled {
            self.seen.lock().remove(&ledger_id);
            return;
        }
        let persisted = self.finish_persisted_unit_with_status(unit, status, error_kind, error);
        self.seen.lock().remove(&ledger_id);
        if persisted && status == "failed" {
            self.mark_failed(&ledger_id);
        }
    }

    pub(crate) fn cancel_unit(&self, unit: &ProbeUnit, reason: &str) {
        let ledger_id = unit.row.file.file_id.to_string();
        self.seen.lock().remove(&ledger_id);
        let Some(job_id) = unit.job_id.as_deref() else {
            return;
        };
        let result = self.store.library_write(|store| {
            store
                .cancel_probe_unit(job_id, &ledger_id, reason)
                .and_then(|(job, _, all_done)| {
                    // A refresh is published atomically; membership changes cancel the entire batch.
                    if job.kind == "marker_refresh" || all_done {
                        store.cancel_probe_job(job_id, reason)?;
                    }
                    store.get_probe_job(job_id)
                })
        });
        match result {
            Ok(Some(job)) => {
                if !job.is_active() {
                    // Enqueue holds seen before Store. Never retain the Store
                    // temporary guard while acquiring seen (including in if let).
                    let units = self.store.probe_job_units(job_id);
                    if let Ok(units) = units {
                        let mut seen = self.seen.lock();
                        for pending in units {
                            seen.remove(&pending.ledger_id);
                        }
                    }
                    timings::log_terminal_job(&self.store_access(), &self.timings, &job);
                }
                tracing::info!(job_id, ledger_id, reason, status = %job.status,
                    elapsed_ms = job.elapsed_ms(now_ms()), "【媒体探测】任务已取消");
            }
            Ok(None) => tracing::error!(job_id, ledger_id, "取消时探测任务不存在"),
            Err(error) => tracing::error!(%error, job_id, ledger_id, "持久化探测任务取消状态失败"),
        }
    }

    pub(crate) fn finish_persisted_unit_with_status(
        &self,
        unit: &ProbeUnit,
        status: &str,
        error_kind: Option<&str>,
        error: Option<&str>,
    ) -> bool {
        let ledger_id = unit.row.file.file_id.to_string();
        let Some(job_id) = unit.job_id.as_deref() else {
            tracing::error!(ledger_id, "探测队列项缺少持久化任务 ID，拒绝写入结果");
            return false;
        };
        let result = self.store.library_write(|store| {
            store.finish_probe_unit_with_status(job_id, &ledger_id, status, error_kind, error)
        });
        let (job, accepted, all_done) = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, ledger_id, job_id, "持久化探测任务结果失败");
                return false;
            }
        };
        if !accepted {
            tracing::warn!(job_id, ledger_id, "忽略已结束或重复提交的探测单元结果");
            return false;
        }
        tracing::info!(
            job_id = %job.id,
            media_id = %job.media_id,
            season = job.season.unwrap_or(1),
            episode = ?single_episode(&unit.row),
            completed = job.completed,
            total = job.total,
            status,
            "【媒体探测】任务单元结果已写入数据库"
        );
        if !all_done {
            return true;
        }
        if job.status == "cancelled" {
            timings::log_terminal_job(&self.store_access(), &self.timings, &job);
            return true;
        }
        if job.failed > 0 {
            if let Err(error) = self.store.library_write(|store| {
                store.finish_probe_job(
                    &job.id,
                    false,
                    job.error.as_deref().or(Some("一个或多个探测单元失败")),
                )
            }) {
                tracing::error!(%error, job_id = %job.id, "持久化探测任务失败状态失败");
            }
            tracing::warn!(
                job_id = %job.id,
                media_id = %job.media_id,
                completed = job.completed,
                total = job.total,
                elapsed_ms = job.elapsed_ms(now_ms()),
                "【探测任务】部分单元失败，保留现有片头片尾结果"
            );
            timings::log_terminal_job(&self.store_access(), &self.timings, &job);
        } else if job.kind == "marker_refresh" {
            marker_jobs::apply_marker_refresh(self, &job, unit);
            timings::log_terminal_job(&self.store_access(), &self.timings, &job);
        } else {
            let final_succeeded = job.status == "succeeded";
            let job_final_status = job.status.clone();
            match self.store.library_write(|store| {
                store.finish_probe_job(&job.id, final_succeeded, job.error.as_deref())
            }) {
                Ok(()) => tracing::info!(
                    job_id = %job.id,
                    media_id = %job.media_id,
                    completed = job.completed,
                    total = job.total,
                    status = %job_final_status,
                    elapsed_ms = job.elapsed_ms(now_ms()),
                    "【媒体探测】持久化任务完成"
                ),
                Err(error) => {
                    tracing::error!(%error, job_id = %job.id, "持久化探测任务成功状态失败")
                }
            }
            timings::log_terminal_job(&self.store_access(), &self.timings, &job);
        }
        true
    }
}
