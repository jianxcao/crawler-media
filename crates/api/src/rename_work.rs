//! Drive durable rename IO with owned plans, taking the Store only for DB steps.
use store::Store;
use store::StoreError;
use store::library_write::RenameDiskReceipt;

pub(crate) fn record_disk(
    store: &Store,
    receipt: &RenameDiskReceipt,
    now: i64,
) -> Result<(), StoreError> {
    let Some(plan) = store.begin_rename_disk_record(receipt)? else {
        return Ok(());
    };
    let observed = plan.observe();
    let decided = store.decide_rename_disk(observed)?;
    let applied = decided.apply()?;
    store.finish_rename_disk(applied, now)
}

pub(crate) fn complete(store: &Store, rename_id: &str, now: i64) -> Result<(), StoreError> {
    let plan = store.plan_file_rename_completion(rename_id)?;
    let observed = plan.observe()?;
    store.complete_observed_file_rename(observed, now)
}

const RECOVERY_WINDOW: usize = 100;

/// Freeze startup ownership before ApiState can serve new rename requests. Only
/// lightweight IDs are captured here; disk work stays in bounded blocking batches
/// and never retains a Store reader or writer across filesystem IO.
pub(crate) fn resume_startup(state: crate::management::ApiState) {
    let now = crate::store::unix_now();
    let ids = match state.store.list_due_file_rename_ids(now) {
        Ok(ids) => ids,
        Err(error) => {
            tracing::error!(%error, "无法捕获启动时的文件改名恢复清单，本轮跳过恢复");
            // Re-querying later could claim a live foreground request. Preserve
            // its ownership even when startup storage observation fails.
            state.scan_coordinator.trigger_next(&state);
            return;
        }
    };
    tokio::spawn(async move {
        let mut completed = 0;
        for window in ids.chunks(RECOVERY_WINDOW) {
            let batch = window.to_vec();
            let shared = state.store.clone();
            // Each captured intent is attempted once. Failed work keeps its
            // durable retry position without extending this startup snapshot.
            let result =
                tokio::task::spawn_blocking(move || resume_batch(&shared, &batch, now)).await;
            match result {
                Ok(Ok(done)) => completed += done,
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

fn resume_batch(store: &Store, ids: &[String], now: i64) -> Result<usize, StoreError> {
    let mut completed = 0;
    for id in ids {
        let result = resume_one(store, id, now);
        match result {
            Ok(()) => completed += 1,
            Err(error) => {
                if let Err(record) = store.record_file_rename_failure(id, now, &error.to_string()) {
                    tracing::error!(rename_id = %id, %record, "recording rename failure failed");
                    return Err(record);
                }
                tracing::error!(rename_id = %id, %error, "pending file rename resume failed");
            }
        }
    }
    Ok(completed)
}

fn resume_one(store: &Store, rename_id: &str, now: i64) -> Result<(), StoreError> {
    let request = store.load_file_rename(rename_id)?;
    if request.phase == "planned" {
        let receipt = store::library_write::execute_rename_disk(&request).inspect_err(|error| {
            let _ = store.record_rename_refusal(rename_id, now, error);
        })?;
        record_disk(store, &receipt, now)?;
    }
    complete(store, rename_id, now)
}
