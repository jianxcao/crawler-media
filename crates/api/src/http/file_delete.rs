//! Keep a file row until its corresponding file is gone.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::Store;

mod claims;
pub(crate) use claims::path_claim;
#[cfg(test)]
mod tests;

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
    let current = {
        let store = store.lock();
        store
            .file_id_at_deletion_path(entry)?
            .map(|id| store.file_coverage_by_id(id.parse()?))
            .transpose()?
            .flatten()
    };
    let file_now = current.as_ref().map(|row| row.file.file_id.to_string());
    let file_changed = match (entry.file_id.as_deref(), file_now.as_deref()) {
        // A different file owns the path now: the queued file was replaced.
        (Some(queued), Some(now)) => queued != now,
        // A file appeared where the queue recorded none: a new import claimed it.
        (None, Some(_)) => true,
        // No file row: either the path never was a Library file, or the accepted
        // deletion intent already removed the row. The bytes are still ours.
        (_, None) => false,
    };
    let source_changed = if let Some(row) = &current {
        match library::observe_source(&row.file.path) {
            Ok(observed) => observed.source_version != row.file.version.source_version,
            Err(library::LibraryError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                false
            }
            Err(error) => {
                return Err(store::StoreError::Invalid(format!(
                    "source observation failed: {error}"
                )));
            }
        }
    } else {
        false
    };
    let identity_changed = match (observed_identity(&entry.path)?, entry.file_size) {
        (Some(_), None) => true,
        (Some((size, inode)), Some(queued_size)) => {
            size != queued_size || matches!((inode, entry.file_inode), (Some(a), Some(b)) if a != b)
        }
        _ => false,
    };
    Ok(file_changed || identity_changed || source_changed)
}

/// Drop a stale queue row; the file and its file row are kept.
fn abandon_queued_deletion(
    store: &parking_lot::Mutex<Store>,
    entry: &store::QueuedFileDeletion,
) -> Result<(), store::StoreError> {
    let path = entry.path.as_str();
    tracing::warn!(path = %path, "待删除路径已被新文件复用，放弃删除");
    store.lock().abandon_file_deletion(entry)
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
        process_queued_deletion(store, &entry, now)?;
    }
    Ok(())
}

/// Execute one just-queued deletion intent now (off the Store lock). The
/// intent stays queued on failure, so the periodic retry and restart recovery
/// finish it. Returns an error message when the bytes are not yet gone.
pub(crate) fn retry_queued_deletions_for(
    store: &parking_lot::Mutex<Store>,
    path: &str,
) -> Result<(), String> {
    let now = crate::job_loop::unix_now();
    let entry = store
        .lock()
        .due_file_deletion(path, now)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("删除意图不存在 {path}"))?;
    process_queued_deletion(store, &entry, now).map_err(|e| e.to_string())?;
    let still_queued = store
        .lock()
        .file_deletion_intent_pending(&entry)
        .map_err(|e| e.to_string())?;

    if still_queued {
        return Err(format!("删除文件失败 {path}，已保留删除意图"));
    }
    Ok(())
}

fn process_queued_deletion(
    store: &parking_lot::Mutex<Store>,
    entry: &store::QueuedFileDeletion,
    now: i64,
) -> Result<(), store::StoreError> {
    let claim = claims::path_claim(&entry.path_key);
    let _guard = claim.lock();
    let path = entry.path.as_str();
    if store.lock().due_file_deletion_intent(entry, now)?.as_ref() != Some(entry) {
        return Ok(());
    }
    match queued_path_was_reused(store, entry) {
        Ok(true) => {
            abandon_queued_deletion(store, entry)?;
            return Ok(());
        }
        Ok(false) => {}
        Err(error) => {
            tracing::error!(path = %path, %error, "读取待删除文件身份失败，保留删除意图并退避");
            store
                .lock()
                .record_file_deletion_failure(entry, now, &error.to_string())?;
            return Ok(());
        }
    }
    let result = remove_physical_file(path);
    let mut completed = false;
    {
        let store = store.lock();
        // A replacement may publish while filesystem work is outside the lock. Never
        // clear its file row after the accepted intent was cancelled or replaced.
        if store.due_file_deletion_intent(entry, now)?.as_ref() != Some(entry) {
            return Ok(());
        }
        match &result {
            Ok(()) => {
                let current = store.file_id_at_deletion_path(entry)?;
                if current
                    .as_deref()
                    .is_some_and(|id| Some(id) != entry.file_id.as_deref())
                {
                    store.abandon_file_deletion(entry)?;
                } else {
                    store.complete_file_deletion(entry)?;
                    completed = true;
                }
            }
            Err(error) => {
                tracing::warn!(path = %path, %error, "删除文件失败，保留在待删除队列稍后重试");
                store.record_file_deletion_failure(entry, now, error)?;
            }
        }
    }
    if completed {
        prune_disk_leftovers(store, std::path::Path::new(path));
    }
    Ok(())
}

/// Removes the deleted source's sidecars (nfo/artwork/subtitles) and the
/// directories they leave empty, up to (never including) its Library root.
/// Filesystem work runs outside the Store lock; failures only log.
pub(crate) fn prune_disk_leftovers(store: &parking_lot::Mutex<Store>, deleted: &std::path::Path) {
    let roots = match store.lock().list_library_roots() {
        Ok(roots) => roots,
        Err(error) => {
            tracing::warn!(path = %deleted.display(), %error, "读取 Library 根失败，跳过附属文件清理");
            return;
        }
    };
    let Some(root) = roots
        .iter()
        .map(|root| root.path.clone())
        .filter(|root| deleted.starts_with(root) && deleted != root)
        .max_by_key(|root| root.as_os_str().len())
    else {
        return;
    };
    let removed = library::sidecar_cleanup::prune_after_delete(deleted, &root);
    if !removed.is_empty() {
        tracing::info!(path = %deleted.display(), removed = removed.len(), "已清理删除源的附属文件与空目录");
    }
}

fn observed_identity(path: &str) -> Result<Option<(u64, Option<u64>)>, store::StoreError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        Some(metadata.ino())
    };
    #[cfg(not(unix))]
    let inode = None;
    Ok(Some((metadata.len(), inode)))
}

#[derive(Default)]
pub(crate) struct FileDeletionReport {
    pub removed: usize,
    pub freed_bytes: u64,
    pub paths: Vec<String>,
    pub errors: Vec<String>,
}

struct ObservedDeletion {
    coverage: domain::FileCoverage,
    identity: Option<(u64, Option<u64>)>,
    size: u64,
}

/// One side-effecting worker owns the filesystem budget through queueing and
/// completion. A cancelled HTTP future cannot detach an unbounded delete task.
pub(crate) async fn delete_coverages(
    state: &crate::management::ApiState,
    coverages: Vec<domain::FileCoverage>,
    reason: &'static str,
) -> Result<FileDeletionReport, Response> {
    if coverages.is_empty() {
        return Ok(FileDeletionReport::default());
    }
    let worker = state.clone();
    state
        .fs_work
        .run_to_completion(move || delete_coverages_blocking(&worker, coverages, reason))
        .await
        .map_err(super::library::browse_work_error)?
}

fn observe_deletion(coverage: domain::FileCoverage) -> Result<ObservedDeletion, Response> {
    let (identity, size) = match library::observe_source(&coverage.file.path) {
        Ok(observed) if observed.source_version == coverage.file.version.source_version => {
            let identity = store::file_identity(&coverage.file.path.to_string_lossy())
                .ok_or_else(|| deletion_conflict("删除前无法确认文件身份"))?;
            (Some(identity), observed.size_bytes.unwrap_or(0))
        }
        Ok(_) => return Err(deletion_conflict("源文件已变化，拒绝删除")),
        Err(library::LibraryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            (None, 0)
        }
        Err(error) => {
            tracing::error!(%error, path=%coverage.file.path.display(), "观察待删除文件失败");
            return Err(deletion_conflict(&error.to_string()));
        }
    };
    Ok(ObservedDeletion {
        coverage,
        identity,
        size,
    })
}

pub(crate) fn same_recorded_source(
    expected: &domain::FileCoverage,
    current: &domain::FileCoverage,
) -> bool {
    let a = &expected.file;
    let b = &current.file;
    a.file_id == b.file_id
        && a.version == b.version
        && a.media_id == b.media_id
        && a.library_id == b.library_id
        && a.root_id == b.root_id
        && a.path == b.path
        && expected.units == current.units
}

fn queue_observed_deletions(
    state: &crate::management::ApiState,
    observed: &[ObservedDeletion],
    reason: &str,
) -> Result<(), Response> {
    super::store_wait::with_bounded_store(state.store(), |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        for row in observed {
            let expected = &row.coverage;
            let current = store
                .file_coverage_by_id(expected.file.file_id)
                .map_err(deletion_store_error)?
                .ok_or_else(|| deletion_conflict("文件已被删除或替换"))?;
            if !same_recorded_source(expected, &current) {
                return Err(deletion_conflict("文件版本或归属已变化，拒绝删除"));
            }
            let library = current.file.library_id.to_string();
            if !store
                .library_accepts_work(&library)
                .map_err(deletion_store_error)?
                || store
                    .library_lifecycle_state(&library)
                    .map_err(deletion_store_error)?
                    != "ready"
            {
                return Err(deletion_conflict("Library 当前不接受文件删除"));
            }
            let media = store
                .get_media(current.file.media_id)
                .map_err(deletion_store_error)?
                .ok_or_else(|| deletion_conflict("Media 已不存在"))?;
            if super::library::library_for_file_strict(store, &current.file, &media).is_none() {
                return Err(deletion_conflict("文件当前不属于已登记的 Library root"));
            }
        }
        for row in observed {
            let path = row.coverage.file.path.to_string_lossy();
            store
                .enqueue_file_deletion_observed(&path, reason, row.identity)
                .map_err(deletion_store_error)?;
            // Only explicit cleanup requests reset an exhausted intent;
            // periodic background drains preserve their backoff.
            store
                .reset_file_deletion_backoff(&path)
                .map_err(deletion_store_error)?;
        }
        Ok(())
    })
    .unwrap_or_else(|| Err(super::store_wait::busy_response()))
}

fn delete_coverages_blocking(
    state: &crate::management::ApiState,
    coverages: Vec<domain::FileCoverage>,
    reason: &str,
) -> Result<FileDeletionReport, Response> {
    let observed = coverages
        .into_iter()
        .map(observe_deletion)
        .collect::<Result<Vec<_>, _>>()?;
    queue_observed_deletions(state, &observed, reason)?;
    let mut report = FileDeletionReport::default();
    for row in observed {
        let path = row.coverage.file.path.to_string_lossy().into_owned();
        report.paths.push(path.clone());
        if let Err(error) = retry_queued_deletions_for(&state.store, &path) {
            tracing::error!(%error, %path, "删除未完成，持久意图保留供重试");
            report.errors.push(error);
            continue;
        }
        match state
            .store
            .lock()
            .file_coverage_by_id(row.coverage.file.file_id)
        {
            Ok(None) => {
                report.removed += 1;
                report.freed_bytes += row.size;
            }
            Ok(Some(_)) => report.errors.push(format!("文件已变化，未删除 {path}")),
            Err(error) => report.errors.push(error.to_string()),
        }
    }
    Ok(report)
}

fn deletion_conflict(message: &str) -> Response {
    crate::http::err(StatusCode::CONFLICT, "file.delete_conflict", message)
}

fn deletion_store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "文件删除持久事实操作失败");
    crate::http::err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

/// Shared Subscribe cleanup uses the same durable and version-checked path as
/// direct Library deletions. Rows remain until physical deletion has completed.
pub(crate) async fn remove_rows(
    state: &crate::management::ApiState,
    coverages: Vec<domain::FileCoverage>,
) -> (usize, Vec<String>) {
    match delete_coverages(state, coverages, "subscription_deleted").await {
        Ok(report) => (report.removed, report.errors),
        Err(response) => (0, vec![format!("文件清理失败 ({})", response.status())]),
    }
}

/// Review L-10: `GET /library/pending-deletions` — deletions that stopped
/// retrying (with their last error) and how many are still queued.
pub(crate) async fn pending_deletions(
    State(state): State<crate::management::ApiState>,
) -> Response {
    match super::store_wait::store_read(&state, pending_deletions_response).await {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

fn pending_deletions_response(store: &Store) -> Response {
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
