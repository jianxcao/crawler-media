use std::sync::atomic::Ordering;
use domain::MediaKind;

use super::{ProbeManager, ProbeUnit, marker_jobs, timings};

impl ProbeManager {
    pub(crate) fn recover_pending_jobs(&self) {
        let jobs = match self.store.lock().recover_probe_jobs() {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::error!(%error, "读取待恢复媒体探测任务失败");
                return;
            }
        };
        for (job, units) in jobs {
            tracing::info!(
                job_id = %job.id,
                media_id = %job.media_id,
                status = %job.status,
                pending = units.iter().filter(|unit| unit.status == "queued").count(),
                completed = job.completed,
                total = job.total,
                "【探测任务】从数据库恢复未完成任务"
            );
            for persisted in units.iter().filter(|unit| unit.status == "queued") {
                let row = self
                    .store
                    .lock()
                    .get_ledger(&persisted.ledger_id.replace('-', ""));
                let row = match row {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        let _ = self.store.lock().finish_probe_unit(
                            &job.id,
                            &persisted.ledger_id,
                            false,
                            Some("媒体台账不存在，无法恢复探测"),
                        );
                        continue;
                    }
                    Err(error) => {
                        tracing::error!(%error, job_id = %job.id, ledger_id = %persisted.ledger_id, "恢复探测任务读取台账失败");
                        if let Err(finish_error) = self.store.lock().finish_probe_unit(
                            &job.id,
                            &persisted.ledger_id,
                            false,
                            Some("恢复任务时读取媒体台账失败"),
                        ) {
                            tracing::error!(%finish_error, job_id = %job.id, ledger_id = %persisted.ledger_id, "持久化恢复失败的任务单元失败");
                        }
                        continue;
                    }
                };
                let kind = match persisted.kind.parse::<MediaKind>() {
                    Ok(kind) => kind,
                    Err(kind) => {
                        tracing::error!(job_id = %job.id, ledger_id = %persisted.ledger_id, %kind, "恢复探测任务媒体类型无效");
                        let _ = self.store.lock().finish_probe_unit(
                            &job.id,
                            &persisted.ledger_id,
                            false,
                            Some("恢复任务时媒体类型无效"),
                        );
                        continue;
                    }
                };
                self.seen.lock().insert(persisted.ledger_id.clone());
                let unit = ProbeUnit {
                    row,
                    kind,
                    force_fingerprint: persisted.force_fingerprint,
                    reuse_fingerprint_cache: persisted.reuse_fingerprint_cache,
                    overwrite_markers: persisted.overwrite_markers,
                    reuse_media_info_cache: persisted.reuse_media_info_cache,
                    marker_refresh_id: (job.kind == "marker_refresh")
                        .then(|| self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed)),
                    job_id: Some(job.id.clone()),
                };
                if self.queue_metadata(unit).is_err() {
                    self.seen.lock().remove(&persisted.ledger_id);
                    tracing::error!(job_id = %job.id, ledger_id = %persisted.ledger_id, "恢复探测任务加入队列失败");
                    let _ = self.store.lock().finish_probe_unit(
                        &job.id,
                        &persisted.ledger_id,
                        false,
                        Some("恢复任务加入内存队列失败"),
                    );
                }
            }
            let refreshed = self.store.lock().get_probe_job(&job.id);
            let refreshed = match refreshed {
                Ok(Some(refreshed)) => refreshed,
                Ok(None) => {
                    tracing::error!(job_id = %job.id, "恢复探测任务后数据库中找不到任务记录");
                    continue;
                }
                Err(error) => {
                    tracing::error!(%error, job_id = %job.id, "恢复探测任务后读取任务状态失败");
                    match self
                        .store
                        .lock()
                        .fail_completed_probe_job(&job.id, "重启恢复时无法读取已完成任务状态")
                    {
                        Ok(true) => {
                            tracing::error!(job_id = %job.id, "已将无法确认状态的完成任务标记失败并释放任务范围")
                        }
                        Ok(false) => {
                            tracing::warn!(job_id = %job.id, "未终止探测任务：任务可能仍有待处理单元或已被其他流程终止")
                        }
                        Err(finish_error) => {
                            tracing::error!(%finish_error, job_id = %job.id, "读取任务状态失败后持久化失败状态也失败")
                        }
                    }
                    continue;
                }
            };
            if refreshed.completed >= refreshed.total && refreshed.is_active() {
                if refreshed.failed > 0 {
                    if let Err(error) = self.store.lock().finish_probe_job(
                        &job.id,
                        false,
                        refreshed.error.as_deref(),
                    ) {
                        tracing::error!(%error, job_id = %job.id, "持久化恢复任务的失败状态失败");
                    }
                } else if refreshed.kind == "marker_refresh" {
                    match self.recovered_marker_refresh_unit(&refreshed) {
                        Ok(unit) => {
                            marker_jobs::apply_marker_refresh(self, &refreshed, &unit);
                            timings::log_terminal_job(&self.store, &self.timings, &refreshed);
                        }
                        Err(error) => {
                            if let Err(finish_error) = self.store.lock().finish_probe_job(
                                &job.id,
                                false,
                                Some("重启恢复时无法读取任务台账"),
                            ) {
                                tracing::error!(%finish_error, job_id = %job.id, "持久化无法恢复的片头片尾任务失败状态失败");
                            }
                            tracing::error!(%error, job_id = %job.id, "完成的片头片尾任务无法恢复，已标记失败并释放任务范围");
                        }
                    }
                } else if let Err(error) = self.store.lock().finish_probe_job(&job.id, true, None) {
                    tracing::error!(%error, job_id = %job.id, "持久化恢复任务的成功状态失败");
                }
            }
        }
    }

    fn recovered_marker_refresh_unit(
        &self,
        job: &crate::store::ProbeJob,
    ) -> Result<ProbeUnit, crate::store::StoreError> {
        let store = self.store.lock();
        let persisted = store
            .probe_job_units(&job.id)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!("units for job {}", job.id))
            })?;
        let row = store
            .get_ledger(&persisted.ledger_id.replace('-', ""))?
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!("ledger {}", persisted.ledger_id))
            })?;
        let kind = persisted
            .kind
            .parse::<MediaKind>()
            .map_err(|_| crate::store::StoreError::Missing(format!("kind {}", persisted.kind)))?;
        Ok(ProbeUnit {
            row,
            kind,
            force_fingerprint: persisted.force_fingerprint,
            reuse_fingerprint_cache: persisted.reuse_fingerprint_cache,
            overwrite_markers: persisted.overwrite_markers,
            reuse_media_info_cache: persisted.reuse_media_info_cache,
            marker_refresh_id: None,
            job_id: Some(job.id.clone()),
        })
    }
}

