use domain::FileCoverage;
use store::Store;

/// Ledger-side source check (no IO); safe to run under the store lock.
pub(crate) fn ensure_source_recorded(store: &Store, row: &FileCoverage) -> Result<(), String> {
    let current = store
        .file_coverage_by_id(row.file.file_id)
        .map_err(|error| {
            tracing::error!(%error, file_id = %row.file.file_id, "读取声纹源文件台账失败");
            format!("source_check_failed: {error}")
        })?;
    let carried = current.as_ref().is_some_and(|current| {
        current.file.path == row.file.path
            && current.file.media_id == row.file.media_id
            && current.file.library_id == row.file.library_id
            && current.file.root_id == row.file.root_id
            && current.file.version == row.file.version
            && current.file.presence == domain::FilePresence::Present
            && current.file.publication == domain::FilePublication::Published
            && current.units == row.units
    });
    if !carried {
        return Err(format!(
            "file_deleted: file {} is no longer available",
            row.file.file_id
        ));
    }
    let owner = row.file.library_id.to_string();
    let current = current
        .as_ref()
        .ok_or_else(|| "file_deleted: source unavailable".to_string())?;
    let declared = store
        .library_root_owning_key(std::path::Path::new(&current.path_key), row.file.kind)
        .map_err(|error| error.to_string())?;
    if declared != Some((owner.clone(), row.file.root_id.clone())) {
        return Err(format!(
            "file_deleted: source ownership changed: {}",
            row.file.file_id
        ));
    }
    if !store
        .library_accepts_work(&owner)
        .map_err(|e| e.to_string())?
        || store
            .library_lifecycle_state(&owner)
            .map_err(|e| e.to_string())?
            != "ready"
    {
        return Err(format!("file_deleted: Library {owner} is unavailable"));
    }
    Ok(())
}

/// Disk-side source check. Must run without holding the store lock.
pub(crate) fn observe_source(row: &FileCoverage) -> Result<(), String> {
    match library::observe_source(&row.file.path) {
        Ok(observed) if observed.source_version == row.file.version.source_version => Ok(()),
        Ok(_) => {
            tracing::warn!(file_id = %row.file.file_id, path = %row.file.path.display(),
                "源文件内容已变化，拒绝发布原版本的探测结果");
            Err(format!(
                "file_deleted: source changed: {}",
                row.file.path.display()
            ))
        }
        Err(library::LibraryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("file_deleted: {}", row.file.path.display()))
        }
        Err(error) => {
            tracing::error!(%error, path = %row.file.path.display(), "检查声纹源文件失败");
            Err(format!("source_check_failed: {error}"))
        }
    }
}

pub(crate) fn is_file_deleted(error: &str) -> bool {
    error.starts_with("file_deleted")
}
