use super::policy::ProbeRequestOrigin;
use super::ProbeManager;

impl ProbeManager {
    /// Claim due failed stages and queue only those stages. Does not read media.
    pub fn dispatch_due(&self, now_ms: i64, limit: usize) -> Result<usize, store::StoreError> {
        let due = self.store.lock().list_due_probe_stages(now_ms, limit)?;
        let mut queued = 0;
        for stage in due {
            let Some(row) = self
                .store
                .lock()
                .get_ledger(&stage.key.ledger_id)
                .unwrap_or_else(|error| {
                    tracing::error!(
                        %error,
                        ledger_id = %stage.key.ledger_id,
                        "【媒体探测】读取到期阶段台账失败"
                    );
                    None
                })
            else {
                continue;
            };
            match self.request_probe(&row, ProbeRequestOrigin::BackgroundRetry) {
                Ok(super::policy::ProbeRequestResult::Queued { job_id }) => {
                    tracing::info!(
                        job_id,
                        ledger_id = %stage.key.ledger_id,
                        stage = stage.key.stage.as_str(),
                        failure_count = stage.failure_count,
                        next_retry_at_ms = stage.next_retry_at_ms,
                        "【媒体探测】到期阶段已重新入队"
                    );
                    queued += 1;
                }
                Ok(_) => {}
                Err(error) => tracing::error!(
                    %error,
                    ledger_id = %stage.key.ledger_id,
                    stage = stage.key.stage.as_str(),
                    "【媒体探测】派发到期阶段失败"
                ),
            }
        }
        Ok(queued)
    }
}
