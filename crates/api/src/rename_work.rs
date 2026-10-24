//! Drive durable rename IO with owned plans, taking the Store only for DB steps.
use parking_lot::Mutex;
use store::Store;
use store::StoreError;
use store::library_write::RenameDiskReceipt;

pub(crate) fn record_disk(
    store: &Mutex<Store>,
    receipt: &RenameDiskReceipt,
    now: i64,
) -> Result<(), StoreError> {
    let Some(plan) = store.lock().begin_rename_disk_record(receipt)? else {
        return Ok(());
    };
    let observed = plan.observe();
    let decided = store.lock().decide_rename_disk(observed)?;
    let applied = decided.apply()?;
    store.lock().finish_rename_disk(applied, now)
}

pub(crate) fn complete(store: &Mutex<Store>, rename_id: &str, now: i64) -> Result<(), StoreError> {
    let plan = store.lock().plan_file_rename_completion(rename_id)?;
    let observed = plan.observe()?;
    store.lock().complete_observed_file_rename(observed, now)
}

const RECOVERY_WINDOW: usize = 100;

/// Keep recovery ahead of scan resumption without blocking a Tokio worker or
/// retaining one Store guard across filesystem work or the whole pending queue.
pub(crate) fn resume_startup(state: crate::management::ApiState) {
    tokio::spawn(async move {
        let mut completed = 0;
        let now = crate::store::unix_now();
        loop {
            let shared = state.store.clone();
            // A fixed pass cutoff prevents a slow batch from retrying its own
            // failed intents again before the initial recovery pass can finish.
            let result = tokio::task::spawn_blocking(move || resume_batch(&shared, now)).await;
            match result {
                Ok(Ok((attempted, done))) => {
                    completed += done;
                    if attempted < RECOVERY_WINDOW {
                        break;
                    }
                }
                Ok(Err(error)) => {
                    tracing::error!(%error, "恢复未完成的文件改名/改挂失败");
                    break;
                }
                Err(error) => {
                    tracing::error!(%error, "文件改名/改挂恢复 worker 失败");
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
        if completed > 0 {
            tracing::info!(count = completed, "已恢复未完成的文件改名/改挂");
        }
        state.scan_coordinator.trigger_next(&state);
    });
}

fn resume_batch(store: &Mutex<Store>, now: i64) -> Result<(usize, usize), StoreError> {
    let ids = store
        .lock()
        .list_due_file_rename_ids_limited(now, RECOVERY_WINDOW)?;
    let attempted = ids.len();
    let mut completed = 0;
    for id in ids {
        let result = resume_one(store, &id, now);
        match result {
            Ok(()) => completed += 1,
            Err(error) => {
                if let Err(record) =
                    store
                        .lock()
                        .record_file_rename_failure(&id, now, &error.to_string())
                {
                    tracing::error!(rename_id = %id, %record, "recording rename failure failed");
                    return Err(record);
                }
                tracing::error!(rename_id = %id, %error, "pending file rename resume failed");
            }
        }
    }
    Ok((attempted, completed))
}

fn resume_one(store: &Mutex<Store>, rename_id: &str, now: i64) -> Result<(), StoreError> {
    let request = store.lock().load_file_rename(rename_id)?;
    if request.phase == "planned" {
        let receipt = store::library_write::execute_rename_disk(&request).inspect_err(|error| {
            let _ = store.lock().record_rename_refusal(rename_id, now, error);
        })?;
        record_disk(store, &receipt, now)?;
    }
    complete(store, rename_id, now)
}
