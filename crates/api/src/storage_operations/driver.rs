use super::*;
use std::time::Duration;
const BATCH_MEDIA: usize = 100;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Start the in-process driver unless one is already running for this operation.
pub(crate) fn spawn_driver(state: &ApiState, operation_id: &str, library_id: &str) {
    if !state.library_work.claim_driver(operation_id) {
        return;
    }
    state.library_work.block(library_id, operation_id);
    spawn_claimed_driver(state, operation_id, library_id);
}

pub(crate) fn spawn_claimed_driver(state: &ApiState, operation_id: &str, library_id: &str) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        state.library_work.release(library_id, operation_id);
        state.library_work.release_driver(operation_id);
        tracing::warn!(%operation_id, %library_id, "无 tokio 运行时承载驱动，等待下次启动恢复");
        return;
    };
    let state = state.clone();
    let operation_id = operation_id.to_string();
    let library_id = library_id.to_string();
    handle.spawn(async move {
        if let Err(error) = drive(&state, &operation_id, &library_id).await {
            tracing::error!(%operation_id, %library_id, phase = failure_phase(&error), %error, "Library 重配置驱动失败");
            if !persist_failure(&state, &operation_id, &library_id, &error).await {
                tracing::error!(%operation_id, %library_id, "失败终态未能持久化；保留 driver claim 与 Library block，等待重启恢复");
                return;
            }
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
            "drain: Library 工作在 {} 秒内未停止，等待下次恢复",
            DRAIN_TIMEOUT.as_secs()
        ));
    }
    if state.job_loop_control().is_stopping() {
        return Ok(());
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
        if state.job_loop_control().is_stopping() {
            return Ok(());
        }
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
            return Err(format!(
                "progress: 第 {batch} 批未推进游标，停止驱动等待下次恢复"
            ));
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
    operation_read(state, |store| store.library_operation_view(operation_id))
        .map_err(|error| error.to_string())
}

fn terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed")
}

fn failure_phase(message: &str) -> &'static str {
    if message.starts_with("drain:") {
        "drain"
    } else if message.starts_with("progress:") {
        "progress"
    } else if message.starts_with("read:") {
        "read"
    } else {
        "batch"
    }
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
    let reservation = state
        .fs_work
        .reserve_until_available()
        .await
        .map_err(|error| {
            format!(
                "{} admission: {error}",
                if advance { "batch" } else { "read" }
            )
        })?;
    reservation
        .run_to_completion(move || {
            if !advance {
                return current_view(&owned, &operation_id)
                    .map_err(|error| format!("read: {error}"));
            }
            // Registry -> Store, with the lease held by the actual blocking worker.
            let _lease = owned
                .library_work
                .acquire_for_reconfiguration(&owned.store, &library_id, &operation_id)
                .map_err(|error| format!("batch lease: {error}"))?;
            let store = owned.store.as_ref();
            store
                .advance_library_operation(
                    &operation_id,
                    owned.playback_clock.now_ms(),
                    BATCH_MEDIA,
                )
                .map_err(|error| format!("batch advance: {error}"))?;
            store
                .library_operation_view(&operation_id)
                .map_err(|error| format!("batch result: {error}"))
        })
        .await
        .map_err(|error| format!("{} worker: {error}", if advance { "batch" } else { "read" }))?
}

const MAX_PERSIST_FAILURE_ATTEMPTS: usize = 5;

async fn persist_failure(
    state: &ApiState,
    operation_id: &str,
    library_id: &str,
    message: &str,
) -> bool {
    let mut write_busy_attempts = 0usize;
    loop {
        if state.job_loop_control().is_stopping() {
            return false;
        }
        let owned = state.clone();
        let operation_id = operation_id.to_string();
        let log_operation_id = operation_id.clone();
        let message = message.to_string();
        let reservation = match state.fs_work.reserve_until_available().await {
            Ok(reservation) => reservation,
            Err(error) => {
                tracing::error!(operation_id = %log_operation_id, %library_id, %error, "Library 操作失败状态落库准入已关闭");
                return false;
            }
        };
        let result = reservation
            .run_to_completion(move || {
                owned.store.fail_library_operation_if_owned(
                    &operation_id,
                    &message,
                    owned.playback_clock.now_ms(),
                )
            })
            .await;
        match result {
            Ok(Ok(changed)) => {
                if !changed {
                    tracing::warn!(operation_id = %log_operation_id, %library_id, "失败收尾未修改记录；保留既有终态或现有 owner");
                }
                return true;
            }
            Ok(Err(crate::store::StoreError::WriteBusy)) => {
                write_busy_attempts += 1;
                if write_busy_attempts >= MAX_PERSIST_FAILURE_ATTEMPTS {
                    tracing::error!(operation_id = %log_operation_id, %library_id, attempts = write_busy_attempts, "Library writer 持续繁忙，放弃失败状态落库并保留恢复状态");
                    return false;
                }
                tracing::warn!(operation_id = %log_operation_id, %library_id, attempt = write_busy_attempts, "Library writer 暂忙，保留 driver claim 并重试失败落库");
            }
            Ok(Err(error)) => {
                tracing::error!(operation_id = %log_operation_id, %library_id, %error, "Library 操作失败状态落库失败；保留 durable cursor 供重启恢复");
                return false;
            }
            Err(error) => {
                tracing::error!(operation_id = %log_operation_id, %library_id, %error, "Library 操作失败状态 worker 失败；保留 durable cursor 供重启恢复");
                return false;
            }
        }
        if state.job_loop_control().is_stopping() {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn release_driver_claims(state: &ApiState, library_id: &str, operation_id: &str) {
    state.library_work.release(library_id, operation_id);
    state.library_work.release_driver(operation_id);
    crate::root_reassign_work::notify(state);
}

async fn finalize(state: &ApiState, library_id: &str, operation_id: &str) {
    if state.job_loop_control().is_stopping() {
        release_driver_claims(state, library_id, operation_id);
        return;
    }
    let reservation = match state.fs_work.reserve_until_available().await {
        Ok(reservation) => reservation,
        Err(error) => {
            tracing::error!(%error, %operation_id, %library_id, "Library 操作驱动清理准入已关闭，直接释放内存租约");
            release_driver_claims(state, library_id, operation_id);
            return;
        }
    };
    let completed = state.clone();
    let library_id_owned = library_id.to_string();
    let operation_id_owned = operation_id.to_string();
    if let Err(error) = reservation
        .run_to_completion(move || {
            release_driver_claims(&completed, &library_id_owned, &operation_id_owned);
        })
        .await
    {
        tracing::error!(%error, %operation_id, %library_id, "Library 操作驱动清理失败，等待重启恢复");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use downloader::MemoryDownloader;
    use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
    use std::sync::Arc;

    struct NoFetch;
    impl Fetcher for NoFetch {
        fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
            Err(IndexerError::Fetch("offline".into()))
        }
    }

    fn test_state(dir: &std::path::Path) -> ApiState {
        let store = crate::Store::open(dir.join("data")).unwrap();
        ApiState::new(
            store,
            "secret".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.join("stage"))),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn persist_failure_and_finalize_exit_on_closed_fs_work_and_write_busy_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(tmp.path());
        assert!(state.library_work.claim_driver("op-1"));
        state.library_work.block("lib-1", "op-1");

        // 1) Closing fs_work must let finalize exit promptly and release claims.
        state.fs_work.close();
        tokio::time::timeout(Duration::from_secs(1), finalize(&state, "lib-1", "op-1"))
            .await
            .expect("finalize must exit promptly when fs_work is closed");
        assert!(state.library_work.claim_driver("op-1"));
        assert!(!state.library_work.has_active_work());

        // 2) Closed fs_work must cause persist_failure to return false promptly.
        let persisted = tokio::time::timeout(
            Duration::from_secs(1),
            persist_failure(&state, "op-1", "lib-1", "boom"),
        )
        .await
        .expect("persist_failure must exit promptly when fs_work is closed");
        assert!(!persisted);

        // 3) Stopping signal must cause persist_failure to return false promptly.
        let tmp2 = tempfile::tempdir().unwrap();
        let state2 = test_state(tmp2.path());
        state2.job_loop_control().request_stop();
        let stopped = tokio::time::timeout(
            Duration::from_secs(1),
            persist_failure(&state2, "op-2", "lib-2", "boom"),
        )
        .await
        .expect("persist_failure must exit promptly when stopping");
        assert!(!stopped);

        // 4) WriteBusy retries must be bounded when Library writer is held.
        let tmp3 = tempfile::tempdir().unwrap();
        let state3 = test_state(tmp3.path());
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let store_arc = state3.store();
        let holder = std::thread::spawn(move || {
            store_arc
                .with_writer(store::Db::Library, |_| {
                    held_tx.send(()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(10));
                    Ok(())
                })
                .unwrap();
        });
        held_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let busy_res = tokio::time::timeout(
            Duration::from_secs(7),
            persist_failure(&state3, "op-3", "lib-3", "boom"),
        )
        .await
        .expect("persist_failure must cap WriteBusy retries instead of looping forever");
        let _ = release_tx.send(());
        holder.join().unwrap();
        assert!(!busy_res);
    }
}
