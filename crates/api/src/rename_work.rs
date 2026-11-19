//! Replay durable rename intents captured at startup.
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use store::{Store, StoreError, unix_now};

const RECOVERY_POLL_SECS: i64 = 5;

/// Capture only preexisting intents before the instance starts serving new writes.
pub(crate) fn resume_startup(state: crate::management::ApiState) {
    let now = unix_now();
    let entries = match state.store.file_rename_recovery_schedule() {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!(%error, "无法捕获启动时的文件改名恢复清单，本轮跳过恢复");
            // Re-querying later could claim a live foreground request.
            state.scan_coordinator.trigger_next(&state);
            return;
        }
    };
    tokio::spawn(drive_recovery(state, now, entries));
}

async fn drive_recovery(
    state: crate::management::ApiState,
    started_at: i64,
    entries: Vec<(i64, String)>,
) {
    let store = Arc::downgrade(&state.store);
    let writable = Arc::downgrade(&state.instance_writable);
    let mut startup = Some(state);
    let mut schedule: BinaryHeap<_> = entries
        .into_iter()
        .enumerate()
        .map(|(position, (due, id))| Reverse((due, position, id)))
        .collect();
    while let Some(Reverse((due, position, id))) = schedule.pop() {
        let (Some(shared), Some(writable)) = (store.upgrade(), writable.upgrade()) else {
            return;
        };
        let can_write = writable.load(Ordering::SeqCst);
        if (due > started_at || !can_write)
            && let Some(state) = startup.take()
        {
            state.scan_coordinator.trigger_next(&state);
        }
        let now = unix_now();
        if due > now || !can_write {
            schedule.push(Reverse((due, position, id)));
            drop(shared);
            drop(writable);
            let wait = due.saturating_sub(now).clamp(1, RECOVERY_POLL_SECS);
            tokio::time::sleep(Duration::from_secs(wait as u64)).await;
            continue;
        }
        let executing = id.clone();
        let result = tokio::task::spawn_blocking(move || {
            if !writable.load(Ordering::SeqCst) {
                return Ok(Some(unix_now().saturating_add(1)));
            }
            resume_one(&shared, &executing)
        })
        .await;
        let retry_at = match result {
            Ok(Ok(retry_at)) => retry_at,
            Ok(Err(error)) => {
                tracing::error!(rename_id = %id, %error, "读取或记录文件改名恢复状态失败");
                Some(unix_now().saturating_add(RECOVERY_POLL_SECS))
            }
            Err(error) => {
                tracing::error!(rename_id = %id, %error, "文件改名恢复 worker 失败");
                Some(unix_now().saturating_add(RECOVERY_POLL_SECS))
            }
        };
        if let Some(retry_at) = retry_at {
            schedule.push(Reverse((retry_at, position, id)));
        }
    }
    if let Some(state) = startup {
        state.scan_coordinator.trigger_next(&state);
    }
}

fn resume_one(store: &Store, id: &str) -> Result<Option<i64>, StoreError> {
    let Some(due) = store.file_rename_retry_at(id)? else {
        return Ok(None);
    };
    let now = unix_now();
    if due > now {
        return Ok(Some(due));
    }
    match store.resume_one_file_rename(id, now) {
        Ok(()) => {
            tracing::info!(rename_id = id, "已恢复未完成的文件改名/改挂");
            Ok(None)
        }
        Err(error) => {
            tracing::error!(rename_id = id, %error, "pending file rename resume failed");
            store.record_file_rename_failure(id, unix_now(), &error.to_string())?;
            store.file_rename_retry_at(id)
        }
    }
}
