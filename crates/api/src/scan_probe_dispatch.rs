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

        let enqueued_or_active = match probe.request_probe(
            &row,
            crate::probe_manager::policy::ProbeRequestOrigin::Detail,
        ) {
            Ok(crate::probe_manager::policy::ProbeRequestResult::Queued { .. }) => {
                probe.holds_dispatch_id(&row.file.file_id.to_string())
            }
            Ok(
                crate::probe_manager::policy::ProbeRequestResult::AlreadyRunning
                | crate::probe_manager::policy::ProbeRequestResult::Complete,
            ) => true,
            _ => false,
        };

        if enqueued_or_active {
            let store = shared.lock();
            match store.ack_scan_probe_obligation(ob.ledger_id) {
                Ok(()) => acked += 1,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        ledger_id = %ob.ledger_id,
                        "确认探测义务失败（该义务将被反复重派）"
                    );
                }
            }
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
