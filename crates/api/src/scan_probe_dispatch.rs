//! Dispatch pending probe obligations bounded at 100 entries per round.

use crate::management::ApiState;

/// Attempt to dispatch up to 100 pending probe obligations.
///
/// Returns the number of dispatched and acknowledged obligations.
pub async fn dispatch_once(state: ApiState) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || dispatch_once_sync(&state))
        .await
        .map_err(|e| e.to_string())?
}

fn dispatch_once_sync(state: &ApiState) -> Result<usize, String> {
    dispatch_obligations(&state.store, &state.probe)
}

fn dispatch_obligations(
    shared: &parking_lot::Mutex<store::Store>,
    probe: &crate::probe_manager::ProbeManager,
) -> Result<usize, String> {
    let obligations = {
        let store = shared.lock();
        store
            .scan_probe_obligations(100)
            .map_err(|e| e.to_string())?
    };

    if obligations.is_empty() {
        return Ok(0);
    }

    let mut acked = 0;
    for ob in obligations {
        let row_opt = {
            let store = shared.lock();
            store
                .file_coverage_by_id(ob.ledger_id)
                .map_err(|e| e.to_string())?
        };

        let Some(row) = row_opt else {
            // Ledger row no longer exists (pruned/deleted), ack obligation immediately
            let store = shared.lock();
            match store.ack_scan_probe_obligation(ob.ledger_id) {
                Ok(()) => acked += 1,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        ledger_id = %ob.ledger_id,
                        "台账行已不存在，确认探测义务失败（该义务将被反复重派）"
                    );
                }
            }
            continue;
        };

        // An obligation means "this file still needs a probe". Once presence has recorded the
        // source missing the work cannot succeed, and keeping the row would fill the head of
        // the 100-row dispatch window with files that will never run. Restoring the source
        // re-arms the obligation (`crates/api/src/library_presence.rs`).
        {
            let store = shared.lock();
            if store
                .ledger_is_missing(&row.file.file_id.to_string())
                .map_err(|error| error.to_string())?
            {
                store
                    .ack_scan_probe_obligation(ob.ledger_id)
                    .map_err(|error| error.to_string())?;
                acked += 1;
                continue;
            }
        }

        let result = probe.request_probe(
            &row,
            crate::probe_manager::policy::ProbeRequestOrigin::Detail,
        );
        // Keep acceptance and ACK under one Store lock so presence cannot cancel
        // accepted work and restore a new obligation between these two writes.
        let store = shared.lock();
        let acknowledged = match result {
            Ok(crate::probe_manager::policy::ProbeRequestResult::Queued { job_id }) => store
                .ack_scan_probe_obligation_for_work(&ob, Some(&job_id))
                .map_err(|error| error.to_string())?,
            Ok(crate::probe_manager::policy::ProbeRequestResult::AlreadyRunning) => store
                .ack_scan_probe_obligation_for_work(&ob, None)
                .map_err(|error| error.to_string())?,
            Ok(crate::probe_manager::policy::ProbeRequestResult::Complete) => store
                .ack_scan_probe_obligation_if_current(&ob)
                .map_err(|error| error.to_string())?,
            Err(error) => {
                tracing::error!(%error, ledger_id = %row.file.file_id, "扫描探测义务派发失败，保留义务等待重试");
                false
            }
            _ => false,
        };

        if acknowledged {
            acked += 1;
        }
    }

    Ok(acked)
}

/// Start background dispatcher task checking obligations every 5 seconds.
pub fn start_dispatcher(state: ApiState) {
    let shared = std::sync::Arc::downgrade(&state.store);
    let probe = std::sync::Arc::downgrade(&state.probe);
    let instance = std::sync::Arc::downgrade(&state.instance_writable);
    tokio::spawn(async move {
        loop {
            if instance.upgrade().is_none() {
                break;
            }
            let (Some(shared), Some(probe)) = (shared.upgrade(), probe.upgrade()) else {
                break;
            };
            let result =
                tokio::task::spawn_blocking(move || dispatch_obligations(&shared, &probe)).await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => tracing::error!(%error,"Probe obligation dispatcher failed"),
                Err(error) => tracing::error!(%error,"Probe obligation dispatcher worker failed"),
            }
            // The sleep retains Weak handles only; dropping an ApiState can stop
            // its drivers and release its per-instance blocking budgets.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}
