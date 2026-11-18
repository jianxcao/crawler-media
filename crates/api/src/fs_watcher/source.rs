//! SQLite source identity decides whether a notification invalidates derived
//! facts. In-memory deletion debounce cannot prove that file content changed.

use std::path::Path;

use crate::management::ApiState;

/// Return whether ingestion/Scrape still needs a scan. Failed observations keep
/// existing facts intact and let the ScanCoordinator retry the source change.
pub(super) fn observe_change(state: &ApiState, path: &Path) -> bool {
    match publish_change(state, path) {
        Ok(changed) => changed,
        Err(error) => {
            tracing::error!(path = %path.display(), %error,
                "Watcher 源版本检查失败，保留派生结果并交给扫描重试");
            true
        }
    }
}

fn publish_change(state: &ApiState, path: &Path) -> Result<bool, store::StoreError> {
    let store = state.store.as_ref();
    let Some(coverage) = store.file_coverage_by_path(&path.to_string_lossy())? else {
        return Ok(true);
    };
    let file = coverage.file;
    let scope = store::library_write::WriteScope {
        library_id: file.library_id,
        config_revision: store.library_config_revision(&file.library_id.to_string())?,
    };
    // Observe outside every Store writer; the transaction below checks both the
    // captured source version and Library configuration before changing facts.
    let observation = match library::observe_source(path) {
        Ok(observation) => observation,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error,
                "Watcher 无法观察 STRM 源，保留派生结果并交给扫描重试");
            return Ok(true);
        }
    };
    if observation.source_version == file.version.source_version {
        return Ok(false);
    }
    let version = store.replace_file_source(
        &scope,
        file.file_id,
        &file.version,
        &observation,
        crate::job_loop::unix_now() * 1000,
    )?;
    tracing::info!(path = %path.display(), file_id = %file.file_id,
        revision = version.revision,
        "Watcher 确认 STRM 源变化，已原子失效派生结果并登记持久探测义务");
    Ok(true)
}
