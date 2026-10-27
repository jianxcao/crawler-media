use super::ProbeManager;
use super::policy::{ProbeRequestOrigin, ProbeRequestResult};

/// A claimed candidate stays leased this long while it is evaluated and queued;
/// the worker's terminal write clears it, an abandoned job re-surfaces after it.
const DISPATCH_LEASE_MS: i64 = 10 * 60 * 1000;
/// Rows examined per call, bounding the work superseded contexts can cost.
const SCAN_BUDGET_FACTOR: usize = 32;

impl ProbeManager {
    /// Queue due retries of the *current* probe context only.
    ///
    /// Candidates are claimed transactionally (leased) in deadline order and
    /// classified before they count against `limit`: superseded contexts and
    /// stages no longer needed are retired durably; current ones are queued once
    /// per file. Claimed rows never come back on the next tick, so stale rows
    /// ahead of a real retry cannot starve it; total work is bounded by
    /// `limit * SCAN_BUDGET_FACTOR` rows.
    pub fn dispatch_due(&self, now_ms: i64, limit: usize) -> Result<usize, store::StoreError> {
        let mut queued = 0usize;
        let mut scanned = 0usize;
        let budget = limit.saturating_mul(SCAN_BUDGET_FACTOR);
        while queued < limit && scanned < budget {
            let batch =
                self.store
                    .lock()
                    .claim_due_probe_stages(now_ms, DISPATCH_LEASE_MS, limit)?;
            if batch.is_empty() {
                break;
            }
            scanned += batch.len();
            let mut seen_files = std::collections::HashSet::new();
            for stage in batch {
                if self.dispatch_one(&stage, now_ms, &mut seen_files) {
                    queued += 1;
                }
            }
        }
        Ok(queued)
    }

    fn dispatch_one(
        &self,
        stage: &store::ProbeStageState,
        now_ms: i64,
        seen_files: &mut std::collections::HashSet<String>,
    ) -> bool {
        let key = &stage.key;
        let retire = |reason: &str| match self.store.lock().retire_probe_stage(key) {
            Ok(_) => tracing::info!(
                ledger_id = %key.ledger_id,
                context = %key.context_key,
                stage = key.stage.as_str(),
                reason,
                "【媒体探测】退役到期阶段"
            ),
            Err(error) => tracing::error!(
                %error,
                ledger_id = %key.ledger_id,
                context = %key.context_key,
                reason,
                "【媒体探测】退役到期阶段失败"
            ),
        };
        let Some(file_id) = super::ledger_id(&key.ledger_id) else {
            retire("unparsable_ledger_id");
            return false;
        };
        // Owned lookup first: the Store guard must be gone before any arm can
        // retire (which locks the same non-reentrant mutex again).
        let loaded = { self.store.lock().file_coverage_by_id(file_id) };
        let row = match loaded {
            Ok(Some(row)) => row,
            Ok(None) => {
                retire("file_deleted");
                return false;
            }
            Err(error) => {
                tracing::error!(%error, ledger_id = %key.ledger_id, "【媒体探测】读取到期阶段台账失败");
                return false;
            }
        };
        let source_version = crate::fingerprint_job::current_source_version(&row.file.path);
        let need = {
            let store = self.store.lock();
            super::policy::assess_probe_need(
                &store,
                &row.file,
                ProbeRequestOrigin::BackgroundRetry,
                now_ms,
                source_version,
            )
        };
        // Without valid metadata the current context is not yet known (duration
        // missing): never retire on that basis; the request re-probes metadata.
        if need.metadata_valid && need.context_key != key.context_key {
            retire("superseded_context");
            return false;
        }
        if !need.fingerprint_enabled {
            // Paused, not resolved: the lease keeps it out of the window.
            return false;
        }
        if !seen_files.insert(key.ledger_id.clone()) {
            return false;
        }
        match self.request_probe(&row, ProbeRequestOrigin::BackgroundRetry) {
            Ok(ProbeRequestResult::Queued { job_id }) => {
                tracing::info!(
                    job_id,
                    ledger_id = %key.ledger_id,
                    context = %key.context_key,
                    stage = key.stage.as_str(),
                    failure_count = stage.failure_count,
                    "【媒体探测】到期阶段已重新入队"
                );
                true
            }
            Ok(ProbeRequestResult::Complete) => {
                retire("no_longer_needed");
                false
            }
            // AlreadyRunning / Waiting / Disabled / Exhausted / Cancelled: not a
            // success; the lease defers it and the attempt's result decides.
            Ok(_) => false,
            Err(error) => {
                tracing::error!(
                    %error,
                    ledger_id = %key.ledger_id,
                    context = %key.context_key,
                    stage = key.stage.as_str(),
                    "【媒体探测】派发到期阶段失败"
                );
                false
            }
        }
    }
}
