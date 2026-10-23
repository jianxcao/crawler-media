use domain::{FileCoverage, UnitKey};
use store::Store;

/// Ledger-side source check (no IO); safe to run under the store lock.
pub(crate) fn ensure_source_recorded(store: &Store, row: &FileCoverage) -> Result<(), String> {
    let current = store
        .file_coverage_by_id(row.file.file_id)
        .map_err(|error| {
            tracing::error!(%error, file_id = %row.file.file_id, "读取声纹源文件台账失败");
            format!("source_check_failed: {error}")
        })?;
    // 单集载体约定：比对目标是**一个**单元，多单元覆盖事实取首个单元（整文件单元为
    // `WHOLE`）。台账里同一路径/媒体且仍带该单元，才算源文件还在。
    let target = super::selected_unit(row).unwrap_or(UnitKey::WHOLE);
    let carried = current.as_ref().is_some_and(|current| {
        current.file.path == row.file.path
            && current.file.media_id == row.file.media_id
            && (current.units.contains(&target) || (target.is_whole() && !current.is_episodic()))
    });
    if !carried {
        return Err(format!(
            "file_deleted: file {} is no longer available",
            row.file.file_id
        ));
    }
    if let Some(media) = store
        .get_media(row.file.media_id)
        .map_err(|e| e.to_string())?
    {
        if let Some(owner) = store
            .library_for_path_strict(&row.file.path, media.kind)
            .map_err(|e| e.to_string())?
        {
            if !store
                .library_accepts_work(&owner.id)
                .map_err(|e| e.to_string())?
            {
                return Err(format!(
                    "file_deleted: Library {} is being removed",
                    owner.id
                ));
            }
        }
    }
    Ok(())
}

/// Disk-side source check. Must run without holding the store lock.
pub(crate) fn observe_source(row: &FileCoverage) -> Result<(), String> {
    match row.file.path.try_exists() {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("file_deleted: {}", row.file.path.display())),
        Err(error) => {
            tracing::error!(%error, path = %row.file.path.display(), "检查声纹源文件失败");
            Err(format!("source_check_failed: {error}"))
        }
    }
}

/// Ledger check under a short lock, then the disk observation outside it.
pub(crate) fn ensure_source_available(
    store: &parking_lot::Mutex<Store>,
    row: &FileCoverage,
) -> Result<(), String> {
    ensure_source_recorded(&store.lock(), row)?;
    observe_source(row)
}

pub(crate) fn is_file_deleted(error: &str) -> bool {
    error.starts_with("file_deleted")
}
