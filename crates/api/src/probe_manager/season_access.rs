//! Owned collaborators for a season run on the existing blocking budget.
use std::sync::Arc;

use domain::MediaId;
use marker::FingerprintEngine;
use marker::fingerprint::capture_types::FingerprintCaptureEngine;
use parking_lot::Mutex;
use store::{MarkerResultReplacement, Store};

use crate::probe_manager::progress::MetadataPriorityGate;
use crate::probe_manager::timings::ProbeTimingLedger;
use crate::probe_manager::{ProbeManager, ProbeStoreAccess, ProbeUnit};

pub(super) struct SeasonAccess {
    pub(super) store: Arc<Store>,
    pub(super) fingerprint_engine: Arc<dyn FingerprintEngine>,
    pub(super) capture_engine: Arc<dyn FingerprintCaptureEngine>,
    pub(super) gate: MetadataPriorityGate,
    pub(super) timings: ProbeTimingLedger,
    cancelled_dispatches: Mutex<Vec<String>>,
}

impl SeasonAccess {
    pub(super) fn from_manager(manager: &ProbeManager) -> Self {
        Self {
            store: manager.store.clone(),
            fingerprint_engine: manager.fingerprint_engine.clone(),
            capture_engine: manager.capture_engine.clone(),
            gate: MetadataPriorityGate::from_manager(manager),
            timings: manager.timings.clone(),
            cancelled_dispatches: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn store_access(&self) -> ProbeStoreAccess {
        ProbeStoreAccess::new(self.store.clone())
    }

    pub(super) fn cancel_unit(&self, unit: &ProbeUnit, reason: &str) {
        let ledger_id = unit.row.file.file_id.to_string();
        self.cancelled_dispatches.lock().push(ledger_id.clone());
        let Some(job_id) = unit.job_id.as_deref() else {
            return;
        };
        let result = self.store.library_write(|store| {
            let (job, _, all_done) = store.cancel_probe_unit(job_id, &ledger_id, reason)?;
            if job.kind == "marker_refresh" || all_done {
                store.cancel_probe_job(job_id, reason)?;
            }
            store.get_probe_job(job_id)
        });
        match result {
            Ok(Some(job)) => {
                if !job.is_active() {
                    if let Ok(units) = self.store.probe_job_units(job_id) {
                        self.cancelled_dispatches
                            .lock()
                            .extend(units.into_iter().map(|unit| unit.ledger_id));
                    }
                    crate::probe_manager::timings::log_terminal_job(
                        &self.store_access(),
                        &self.timings,
                        &job,
                    );
                }
                tracing::info!(job_id, ledger_id, reason, status = %job.status, "【媒体探测】整季源任务已取消");
            }
            Ok(None) => tracing::error!(job_id, ledger_id, "整季取消时探测任务不存在"),
            Err(error) => tracing::error!(%error, job_id, ledger_id, "持久化整季探测取消状态失败"),
        }
    }
}

/// Run the complete season pipeline away from the caller's Tokio worker.
/// Its owned snapshot shares the manager's readers, writer and cancellation gate.
pub async fn run_adaptive_season_pipeline(
    manager: &ProbeManager,
    job_id: &str,
    media_id: MediaId,
    season: u32,
    units: &[ProbeUnit],
) -> Result<MarkerResultReplacement, String> {
    let access = SeasonAccess::from_manager(manager);
    let job_id = job_id.to_string();
    let units = units.to_vec();
    let runtime = tokio::runtime::Handle::current();
    let reservation = manager.dispatch_work.try_reserve().map_err(|error| {
        format!("probe Store busy: season blocking capacity unavailable: {error}")
    })?;
    let (result, cancelled) = reservation
        .run_to_completion(move || {
            let result = runtime.block_on(super::run_adaptive_season_pipeline_inner(
                &access, &job_id, media_id, season, &units,
            ));
            (result, access.cancelled_dispatches.into_inner())
        })
        .await
        .map_err(|error| format!("season worker failed: {error}"))?;
    let mut seen = manager.seen.lock();
    for ledger_id in cancelled {
        seen.remove(&ledger_id);
    }
    result
}
