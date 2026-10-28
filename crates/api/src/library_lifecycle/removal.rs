use std::sync::Arc;
use std::time::Duration;

use crate::management::ApiState;

pub(super) async fn drain(state: &ApiState, id: &str) -> Result<(), String> {
    let owned = id.to_owned();
    let operation = window(state, move |store| store.library_removal(&owned))
        .await?
        .ok_or("Library removal missing")?;
    if operation.state == "succeeded" {
        return Ok(());
    }
    let library = operation.library_id;
    let owned = id.to_owned();
    window(state, move |store| {
        store.set_removal_state(&owned, "draining", None)
    })
    .await?;
    tracing::info!(
        operation_id = id,
        library_id = library,
        "开始排空 Library 关联任务与活跃会话"
    );
    let work = state.library_work.clone();
    let library_for_cancel = library.clone();
    tokio::task::spawn_blocking(move || work.cancel(&library_for_cancel))
        .await
        .map_err(|e| e.to_string())?;
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
    wait_work_stopped(state, id, &library).await?;
    // No lease or accepted publication can recreate a cover after freeze.
    // Finish the filesystem side effect before a DB worker can publish succeeded.
    remove_covers(state, &library).await?;
    let owned = id.to_owned();
    window(state, move |store| {
        store.set_removal_state(&owned, "cleaning", None)
    })
    .await?;
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

/// How often a slow drain re-cancels and reports progress.
const DRAIN_SLICE: Duration = Duration::from_secs(30);

/// Waits until every lease on the Library is released. Heavy Libraries can have
/// work that needs longer than any fixed window to observe cancellation, so slow
/// work never fails the removal (it used to flip to `failed`, leaving the Library
/// hidden with all rows intact). Each slice re-cancels late leases and records the
/// wait on the operation so clients can show it.
async fn wait_work_stopped(state: &ApiState, id: &str, library: &str) -> Result<(), String> {
    let started = tokio::time::Instant::now();
    loop {
        if tokio::time::timeout(DRAIN_SLICE, state.library_work.wait_idle(library))
            .await
            .is_ok()
        {
            return Ok(());
        }
        let waited = started.elapsed().as_secs();
        tracing::warn!(
            operation_id = id,
            library_id = library,
            waited_secs = waited,
            "Library 删除仍在等待活跃任务退出，重新发出取消"
        );
        let work = state.library_work.clone();
        let owned_library = library.to_owned();
        tokio::task::spawn_blocking(move || work.cancel(&owned_library))
            .await
            .map_err(|e| e.to_string())?;
        let owned = id.to_owned();
        let note = format!("waiting for active Library work to stop ({waited}s)");
        window(state, move |store| {
            store.set_removal_state(&owned, "draining", Some(&note))
        })
        .await?;
    }
}

async fn remove_covers(state: &ApiState, library: &str) -> Result<(), String> {
    let covers = window(state, |store| Ok(store.library_covers_dir())).await?;
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
        after = files.last().cloned().unwrap_or(after);
        let probe = state.probe.clone();
        tokio::task::spawn_blocking(move || probe.cancel_ledger_work(&files, "Library removed"))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
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
