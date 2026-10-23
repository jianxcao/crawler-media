use axum::http::StatusCode;
use axum::response::Response;
use std::path::PathBuf;

use crate::http::err;
use crate::store::Library;

/// The Library that persistently owns `file` (its `library_id`), never inferred
/// from path roots or defaulted. A failed lookup is logged and owns nothing.
pub(crate) fn library_for_file(
    store: &crate::Store,
    file: &domain::LibraryFile,
    _media: &domain::Media,
) -> Option<Library> {
    match store.get_library(&file.library_id.to_string()) {
        Ok(library) => library,
        Err(error) => {
            tracing::error!(%error, file_id = %file.file_id, "file owner library lookup failed");
            None
        }
    }
}

/// Destructive file operations need the persisted owner *and* a path that still
/// lies strictly inside that owner's roots; there is no default fallback.
pub(crate) fn library_for_file_strict(
    store: &crate::Store,
    file: &domain::LibraryFile,
    media: &domain::Media,
) -> Option<Library> {
    let owner = library_for_file(store, file, media)?;
    let by_path = store
        .library_for_path_strict(&file.path, media.kind)
        .ok()
        .flatten()?;
    (by_path.id == owner.id).then_some(owner)
}

pub(crate) fn file_visible_to_user(
    store: &crate::Store,
    file: &domain::LibraryFile,
    media: &domain::Media,
    user_id: Option<domain::UserId>,
) -> bool {
    library_for_file(store, file, media)
        .is_some_and(|library| library_visible(store, &library, user_id))
}

/// Whether a library is visible to the given user: admins see everything
/// (subject to the library's admin_visible flag for browse scope); members see
/// "everyone" libraries plus "selected" ones that list them.
pub(crate) fn library_visible(
    store: &crate::Store,
    library: &Library,
    user_id: Option<domain::UserId>,
) -> bool {
    if !store.library_accepts_work(&library.id).unwrap_or(false) {
        return false;
    }
    let Some(user_id) = user_id else {
        return true;
    };
    if store
        .user_role(user_id)
        .as_deref()
        .map(|r| r == "admin")
        .unwrap_or(false)
    {
        return library.admin_visible;
    }
    if library.access_mode == "everyone" {
        return true;
    }
    library.member_ids.contains(&user_id)
}

pub(crate) fn library_visible_with_role(
    store: &crate::Store,
    library: &Library,
    user_id: Option<domain::UserId>,
    is_admin: bool,
) -> bool {
    if !store.library_accepts_work(&library.id).unwrap_or(false) {
        return false;
    }
    let Some(user_id) = user_id else {
        return true;
    };
    if is_admin {
        return library.admin_visible;
    }
    library.access_mode == "everyone" || library.member_ids.contains(&user_id)
}

pub(crate) fn missing_library() -> Response {
    err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在")
}

pub(crate) fn artwork_url(kind: &str, ledger_id: domain::LedgerId) -> String {
    format!("/{kind}/{ledger_id}")
}

pub(crate) fn require_visible_library(
    store: &crate::Store,
    id: &str,
    user_id: Option<domain::UserId>,
) -> Result<Library, Response> {
    let Some(library) = store.get_library(id).ok().flatten() else {
        return Err(missing_library());
    };
    if !library_visible(store, &library, user_id) {
        return Err(missing_library());
    }
    Ok(library)
}

/// 长图（fanart.jpg）向上查找：与 poster_path 同一套布局兼容。
pub(crate) fn backdrop_path(file: &domain::LibraryFile) -> Option<PathBuf> {
    backdrop_path_with(file, &crate::browse_file_read::LocalBrowseFileRead)
}

/// 与 [`backdrop_path`] 同一套布局，存在性判断走注入的浏览文件 seam。
pub(crate) fn backdrop_path_with(
    file: &domain::LibraryFile,
    files: &dyn crate::browse_file_read::BrowseFileRead,
) -> Option<PathBuf> {
    let file_dir = file.path.parent()?;
    for dir in [file_dir, file_dir.parent().unwrap_or(file_dir)] {
        let art = dir.join("fanart.jpg");
        if files.is_file(&art) {
            return Some(art);
        }
    }
    None
}

/// 解析媒体库封面：**只读**已存在的版本文件，不生成、不写盘、不写 DB。
/// 自动生成统一走 `LibraryCoverService::ensure_cover`。
pub(crate) fn library_cover_path(_store: &crate::Store, library: &Library) -> Option<PathBuf> {
    // 0. 用户显式删除（"none"/空串）时不回填
    match library.cover_path.as_deref() {
        Some("none") | Some("") => return None,
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        None => {}
    }
    None
}

fn library_cover_checked_key(library_id: &str) -> String {
    format!("library_cover_checked.{library_id}")
}

pub(crate) fn clear_library_cover_checked(store: &crate::Store, library_id: &str) {
    let key = library_cover_checked_key(library_id);
    match store.get_setting(&key) {
        Ok(Some(value)) if value == "auto-v2" => {
            if let Err(error) = store.delete_setting(&key) {
                tracing::warn!(%error, library_id, "failed to clear library cover check marker");
            }
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, library_id, "failed to read library cover check marker");
        }
    }
}
