//! Keep a file row until its corresponding file is gone.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::Store;

fn remove_physical_file(path: &str) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("删除文件失败 {path}: {error}")),
    }
}

/// Review M-3: the path may have been reused by a new import since the
/// deletion was queued (other file row, or a file with another size/inode).
///
/// A storage error is reported instead of guessing: an unreadable identity must
/// never be mistaken for "the queued file is still there".
fn queued_path_was_reused(
    store: &parking_lot::Mutex<Store>,
    entry: &store::QueuedFileDeletion,
) -> Result<bool, store::StoreError> {
    let file_now = store.lock().file_id_at_path(&entry.path)?;
    let file_changed = match (entry.file_id.as_deref(), file_now.as_deref()) {
        // A different file owns the path now: the queued file was replaced.
        (Some(queued), Some(now)) => queued != now,
        // A file appeared where the queue recorded none: a new import claimed it.
        (None, Some(_)) => true,
        // No file row: either the path never was a Library file, or the accepted
        // deletion intent already removed the row. The bytes are still ours.
        (_, None) => false,
    };
    let identity_changed = match (store::file_identity(&entry.path), entry.file_size) {
        (Some((size, inode)), Some(queued_size)) => {
            size != queued_size || matches!((inode, entry.file_inode), (Some(a), Some(b)) if a != b)
        }
        _ => false,
    };
    Ok(file_changed || identity_changed)
}

/// Drop a stale queue row; the file and its file row are kept.
fn abandon_queued_deletion(store: &parking_lot::Mutex<Store>, path: &str) {
    tracing::warn!(path = %path, "待删除路径已被新文件复用，放弃删除");
    if let Err(error) = store.lock().abandon_file_deletion(path) {
        tracing::error!(path = %path, %error, "更新待删除文件队列失败");
    }
}

/// M6: retry every queued Library file deletion. Filesystem work happens
/// outside the Store lock; the queue and file rows are only cleared once the
/// file is really gone.
///
/// Queue read failures and queue write failures are returned to the caller so a
/// broken storage layer cannot look like "nothing to delete".
pub(crate) fn retry_queued_deletions(
    store: &parking_lot::Mutex<Store>,
) -> Result<(), store::StoreError> {
    let now = crate::job_loop::unix_now();
    // Review M-3: only rows whose backoff expired. The Store lock is released
    // at the end of this statement, before any filesystem work below.
    let queued = { store.lock().list_due_file_deletions(now)? };
    for entry in queued {
        let path = entry.path.as_str();
        match queued_path_was_reused(store, &entry) {
            Ok(true) => {
                abandon_queued_deletion(store, path);
                continue;
            }
            Ok(false) => {}
            Err(error) => {
                // Identity unknown: skip this path rather than delete bytes that
                // may belong to a new import.
                tracing::error!(path = %path, %error, "读取待删除文件身份失败，跳过本轮删除");
                continue;
            }
        }
        let result = remove_physical_file(path);
        let store = store.lock();
        match &result {
            Ok(()) => store.complete_file_deletion(path)?,
            Err(error) => {
                tracing::warn!(path = %path, %error, "删除被替换文件失败，保留在待删除队列稍后重试");
                store.record_file_deletion_failure(path, now, error)?;
            }
        }
    }
    Ok(())
}

/// Remove a library file, then remove its file row. A missing file is an
/// already-completed deletion, while every other filesystem error leaves the
/// row intact so callers can report and retry it.
pub(crate) fn remove_file_and_ledger(store: &Store, path: &str) -> Result<(), String> {
    remove_physical_file(path)?;
    store
        .delete_ledger_path(path)
        .map_err(|error| format!("删除记录失败 {path}: {error}"))
}

/// Delete files without holding the global Store mutex, then remove the
/// corresponding file rows in one short database critical section.
pub(crate) async fn remove_rows(
    state: &crate::management::ApiState,
    rows: Vec<domain::LedgerRow>,
) -> (usize, Vec<String>) {
    let paths: Vec<String> = rows.into_iter().map(|row| row.path).collect();
    let delete_paths = paths.clone();
    let physical = tokio::task::spawn_blocking(move || {
        delete_paths
            .into_iter()
            .map(|path| {
                let result = remove_physical_file(&path);
                (path, result)
            })
            .collect::<Vec<_>>()
    })
    .await;
    let physical = match physical {
        Ok(results) => results,
        Err(error) => {
            let message = format!("文件清理任务失败: {error}");
            tracing::error!(%error, "媒体库文件清理任务失败");
            return (0, vec![message]);
        }
    };
    let store = state.store.lock();
    let mut removed = 0usize;
    let mut errors = Vec::new();
    for (path, result) in physical {
        if let Err(error) = result {
            tracing::error!(path, %error, "无法删除媒体库文件");
            errors.push(error);
            continue;
        }
        match store.delete_ledger_path(&path) {
            Ok(()) => removed += 1,
            Err(error) => {
                tracing::error!(path, %error, "无法删除媒体库文件记录");
                errors.push(format!("删除记录失败 {path}: {error}"));
            }
        }
    }
    drop(store);
    (removed, errors)
}

/// Review L-10: `GET /library/pending-deletions` — deletions that stopped
/// retrying (with their last error) and how many are still queued.
pub(crate) async fn pending_deletions(State(state): State<crate::management::ApiState>) -> Response {
    let store = state.store.lock();
    let queued = match store.list_pending_file_deletions() {
        Ok(queued) => queued.len(),
        Err(error) => {
            return crate::http::err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let stuck = match store.list_stuck_file_deletions() {
        Ok(stuck) => stuck,
        Err(error) => {
            return crate::http::err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let stuck: Vec<_> = stuck
        .into_iter()
        .map(|(path, error)| serde_json::json!({ "path": path, "last_error": error }))
        .collect();
    crate::http::ok(serde_json::json!({ "queued": queued, "stuck": stuck })).into_response()
}
