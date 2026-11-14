//! Transfer destination and mode selection.
use domain::MediaKind;

pub fn parse_transfer_mode(raw: &str) -> Option<library::TransferMode> {
    match raw {
        "hardlink" => Some(library::TransferMode::Hardlink),
        "copy" => Some(library::TransferMode::Copy),
        "move" => Some(library::TransferMode::Move),
        _ => None,
    }
}

pub fn transfer_plan(
    store: &crate::Store,
    kind: MediaKind,
) -> Result<(std::path::PathBuf, Option<library::TransferMode>, bool), crate::store::StoreError> {
    transfer_plan_for_library(store, kind, None)
}

pub fn transfer_plan_for_library(
    store: &crate::Store,
    kind: MediaKind,
    library_id: Option<&str>,
) -> Result<(std::path::PathBuf, Option<library::TransferMode>, bool), crate::store::StoreError> {
    let root = if let Some(id) = library_id {
        if let Some(lib) = store.get_library(id)? {
            if !store.library_accepts_work(id)? {
                return Err(crate::store::StoreError::Protected(
                    "目标 Library 正在删除".into(),
                ));
            }
            if let Some(r) = lib.root_paths.first() {
                r.clone()
            } else {
                return Err(crate::store::StoreError::Missing(
                    "目标 Library 没有根目录".into(),
                ));
            }
        } else {
            return Err(crate::store::StoreError::Missing(
                "目标 Library 已删除，请重新选择".into(),
            ));
        }
    } else {
        store.library_root(kind)?
    };
    let mode = parse_transfer_mode(&store.transfer_mode()?);
    Ok((root, mode, store.scrape_enabled()?))
}
