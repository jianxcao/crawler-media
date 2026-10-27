use std::sync::Arc;
use std::time::Duration;

use crate::management::ApiState;

pub(super) async fn drain(state: &ApiState, id: &str) -> Result<(), String> {
    let operation = state
        .store
        .lock()
        .library_removal(id)
        .map_err(|e| e.to_string())?
        .ok_or("Library removal missing")?;
    if operation.state == "succeeded" {
        return Ok(());
    }
    let library = operation.library_id;
    state
        .store
        .lock()
        .set_removal_state(id, "draining", None)
        .map_err(|e| e.to_string())?;
    tracing::info!(
        operation_id = id,
        library_id = library,
        "开始排空 Library 关联任务与活跃会话"
    );
    state.library_work.cancel(&library);
    loop {
        let owned = id.to_owned();
        if window(state, move |store| {
            store.capture_library_removal_batch(&owned)
        })
        .await?
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    cancel_probes(state, id).await?;
    close_remaining_sessions(state, id).await?;
    tokio::time::timeout(
        Duration::from_secs(30),
        state.library_work.wait_idle(&library),
    )
    .await
    .map_err(|_| "Library work did not stop within 30 seconds".to_owned())?;
    // No lease or accepted publication can recreate a cover after freeze.
    // Finish the filesystem side effect before a DB worker can publish succeeded.
    remove_covers(state, &library).await?;
    state
        .store
        .lock()
        .set_removal_state(id, "cleaning", None)
        .map_err(|e| e.to_string())?;
    loop {
        let owned = id.to_owned();
        let result = window(state, move |store| {
            store.clean_library_removal_batch(&owned)
        })
        .await?;
        if result.state == "succeeded" {
            break;
        }
        tokio::task::yield_now().await;
    }
    tracing::info!(
        operation_id = id,
        library_id = library,
        "Library 索引清理完成，删除操作成功"
    );
    Ok(())
}

async fn remove_covers(state: &ApiState, library: &str) -> Result<(), String> {
    let covers = state.store.lock().library_covers_dir();
    loop {
        let directory = covers.clone();
        let library = library.to_owned();
        match state
            .fs_work
            .run_to_completion(move || {
                crate::library_cover_service::remove_library_cover_files(&directory, &library)
            })
            .await
        {
            Ok(_) => return Ok(()),
            Err(crate::blocking_budget::BlockingError::Busy) => {
                tokio::time::sleep(Duration::from_millis(10)).await
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

async fn close_remaining_sessions(state: &ApiState, id: &str) -> Result<(), String> {
    loop {
        let owned = id.to_owned();
        if window(state, move |store| {
            store.close_playback_sessions_for_removal_batch(&owned)
        })
        .await?
            < 100
        {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

async fn cancel_probes(state: &ApiState, id: &str) -> Result<(), String> {
    let mut after = String::new();
    loop {
        let owned = id.to_owned();
        let cursor = after.clone();
        let files = window(state, move |store| {
            store.removal_ledger_ids_after(&owned, &cursor)
        })
        .await?;
        if files.is_empty() {
            return Ok(());
        }
        state
            .probe
            .cancel_ledger_work(&files, "Library removed")
            .map_err(|e| e.to_string())?;
        after = files.last().cloned().unwrap_or(after);
        tokio::task::yield_now().await;
    }
}

/// A worker owns exactly one finite DB window. Capacity contention retries before
/// starting; once admitted, side effects are awaited to completion.
async fn window<T: Send + 'static>(
    state: &ApiState,
    work: impl Fn(&store::Store) -> Result<T, store::StoreError> + Send + Sync + 'static,
) -> Result<T, String> {
    let work = Arc::new(work);
    loop {
        let shared = state.store.clone();
        let work = work.clone();
        match state
            .fs_work
            .run_to_completion(move || work(&shared.lock()))
            .await
        {
            Ok(result) => return result.map_err(|e| e.to_string()),
            Err(crate::blocking_budget::BlockingError::Busy) => {
                tokio::time::sleep(Duration::from_millis(10)).await
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}
