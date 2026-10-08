use std::sync::atomic::Ordering;

use super::{ProbeManager, ProbeUnit, probe};

impl ProbeManager {
    pub(super) fn queue_metadata(&self, unit: ProbeUnit) -> Result<(), ProbeUnit> {
        self.metadata_pending.fetch_add(1, Ordering::AcqRel);
        self.metadata_tx.send(unit).map_err(|error| {
            self.metadata_stage_finished();
            error.0
        })
    }

    pub(super) fn enqueue_fingerprint_after_metadata(
        &self,
        mut work: probe::FingerprintWork,
    ) -> bool {
        let ledger_id = work.unit.row.id.to_string();
        let Some(current_job_id) = work.unit.job_id.clone() else {
            tracing::error!(ledger_id, "媒体信息已完成，但声纹任务缺少持久化任务 ID");
            self.seen.lock().remove(&ledger_id);
            return false;
        };
        let current_job = match self.store.lock().get_probe_job(&current_job_id) {
            Ok(Some(job)) => job,
            Ok(None) => {
                tracing::error!(
                    job_id = current_job_id,
                    ledger_id,
                    "查不到媒体信息任务，声纹未入队"
                );
                self.finish(&work.unit, false);
                return false;
            }
            Err(error) => {
                tracing::error!(%error, job_id = current_job_id, ledger_id, "读取媒体信息任务状态失败，声纹未入队");
                self.finish(&work.unit, false);
                return false;
            }
        };

        if current_job.kind == "media_probe" {
            self.finish_persisted_unit(&work.unit, true);
            let metadata_succeeded = match self.store.lock().get_probe_job(&current_job_id) {
                Ok(Some(job)) => job.status == "succeeded",
                Ok(None) => false,
                Err(error) => {
                    tracing::error!(%error, job_id = current_job_id, ledger_id, "确认媒体信息任务结果失败");
                    false
                }
            };
            if !metadata_succeeded {
                tracing::error!(
                    job_id = current_job_id,
                    ledger_id,
                    "媒体信息已探测，但任务结果未能成功持久化，停止声纹入队"
                );
                self.seen.lock().remove(&ledger_id);
                return false;
            }
            let Some(fingerprint_job_id) =
                self.persist_single_unit(&work.unit, "fingerprint_probe")
            else {
                tracing::warn!(
                    job_id = current_job_id,
                    ledger_id,
                    "媒体信息已成功入库，但创建声纹任务失败，可稍后重试"
                );
                self.seen.lock().remove(&ledger_id);
                self.mark_failed(&ledger_id);
                return false;
            };
            work.unit.job_id = Some(fingerprint_job_id.clone());
            work.start_job = true;
            tracing::info!(
                metadata_job_id = current_job_id,
                fingerprint_job_id,
                ledger_id,
                "【媒体信息】已独立完成；【声纹】已创建可单独追踪的后台任务"
            );
        }

        match self.fingerprint_tx.send(work) {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(ledger_id, "声纹 worker 已退出，无法继续处理后台任务");
                self.finish(&error.0.unit, false);
                false
            }
        }
    }

    pub fn metadata_stage_finished(&self) {
        match self
            .metadata_pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                pending.checked_sub(1)
            }) {
            Ok(1) => self.metadata_idle.notify_waiters(),
            Ok(_) => {}
            Err(_) => tracing::error!("媒体信息队列待处理计数异常：完成时计数已为 0"),
        }
    }
}
