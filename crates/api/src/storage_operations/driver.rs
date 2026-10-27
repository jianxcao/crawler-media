use super::*;
use std::time::Duration;
const BATCH_MEDIA: usize = 100;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Start the in-process driver unless one is already running for this operation.
pub(crate) fn spawn_driver(state: &ApiState, operation_id: &str, library_id: &str) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(%operation_id, %library_id, "无 tokio 运行时承载驱动，等待下次启动恢复");
        return;
    };
    if !state.library_work.claim_driver(operation_id) {
        return;
    }
    let state = state.clone();
    let operation_id = operation_id.to_string();
    let library_id = library_id.to_string();
    handle.spawn(async move {
        if let Err(error) = drive(&state, &operation_id, &library_id).await {
            // The Store keeps the row resumable; startup recovery (or the next 202) retries.
            tracing::error!(%operation_id, %library_id, %error, "Library 重配置驱动中断");
        }
        finalize(&state, &library_id, &operation_id).await;
    });
}

/// Freeze, drain, then apply batches until the operation is terminal.
async fn drive(state: &ApiState, operation_id: &str, library_id: &str) -> Result<(), String> {
    if tokio::time::timeout(DRAIN_TIMEOUT, state.library_work.wait_idle(library_id))
        .await
        .is_err()
    {
        return Err(format!(
            "Library 工作在 {} 秒内未停止，等待下次恢复",
            DRAIN_TIMEOUT.as_secs()
        ));
    }
    let Some(mut current) = driver_step(state, operation_id, library_id, false).await? else {
        tracing::warn!(%operation_id, "重配置操作不存在，驱动退出");
        return Ok(());
    };
    if terminal(&current.state)
        && !(current.state == "failed"
            && matches!(
                current.kind.as_str(),
                "roots" | "locale" | "integrity" | "default_locale"
            ))
    {
        tracing::info!(%operation_id, %library_id, state = %current.state, "重配置操作已是终态");
        return Ok(());
    }
    tracing::info!(
        %operation_id,
        %library_id,
        kind = %current.kind,
        "开始应用 Library 重配置批次"
    );
    let mut batch: u32 = 0;
    loop {
        batch += 1;
        let Some(next) = driver_step(state, operation_id, library_id, true).await? else {
            return Err("Library operation disappeared during batch".into());
        };
        let status = &next;
        if terminal(&status.state) {
            tracing::info!(
                %operation_id,
                %library_id,
                batches = batch,
                state = %status.state,
                "Library 重配置结束"
            );
            return Ok(());
        }
        // The cursor is the only proof a batch moved something; without it the driver
        // would spin forever on a stuck operation.
        let cursor = next.cursor;
        if cursor == current.cursor {
            return Err(format!("第 {batch} 批未推进游标，停止驱动等待下次恢复"));
        }
        current.cursor = cursor;
        tracing::debug!(%operation_id, %library_id, batches = batch, cursor = ?current.cursor, "Library 重配置批次已应用");
        tokio::task::yield_now().await;
    }
}

pub(super) fn current_view(
    state: &ApiState,
    operation_id: &str,
) -> Result<Option<LibraryOperationView>, String> {
    state
        .store
        .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
        .ok_or_else(|| "Store busy while reading Library operation".to_string())?
        .library_operation_view(operation_id)
        .map_err(|error| error.to_string())
}

fn terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed")
}

async fn driver_step(
    state: &ApiState,
    operation_id: &str,
    library_id: &str,
    advance: bool,
) -> Result<Option<LibraryOperationView>, String> {
    let owned = state.clone();
    let operation_id = operation_id.to_string();
    let library_id = library_id.to_string();
    state
        .fs_work
        .run_to_completion(move || {
            if !advance {
                return current_view(&owned, &operation_id);
            }
            // Registry -> Store, with the lease held by the actual blocking worker.
            let _lease = owned.library_work.acquire_for_reconfiguration(
                &owned.store,
                &library_id,
                &operation_id,
            )?;
            let store = owned
                .store
                .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
                .ok_or_else(|| "Store busy while advancing Library operation".to_string())?;
            store
                .advance_library_operation(
                    &operation_id,
                    owned.playback_clock.now_ms(),
                    BATCH_MEDIA,
                )
                .map_err(|error| error.to_string())?;
            store
                .library_operation_view(&operation_id)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())?
}

async fn finalize(state: &ApiState, library_id: &str, operation_id: &str) {
    loop {
        let completed = state.clone();
        let library_id = library_id.to_string();
        let operation_id = operation_id.to_string();
        let result = state
            .fs_work
            .run_to_completion(move || {
                completed.library_work.release(&library_id, &operation_id);
                completed.library_work.release_driver(&operation_id);
                crate::root_reassign_work::notify(&completed);
            })
            .await;
        match result {
            Ok(()) => return,
            Err(crate::blocking_budget::BlockingError::Busy) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => {
                tracing::error!(%error, "Library 操作驱动清理失败，等待重启恢复");
                return;
            }
        }
    }
}
