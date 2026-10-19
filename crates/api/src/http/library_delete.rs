//! Delete a Library item: remove its file rows and, through the durable deletion
//! queue, the bytes under library roots.
//!
//! Removing the last file of a Library item removes only what that Library derived
//! from it (`library_items`/`library_units`/`library_item_terms`, reduced inside
//! `Store::delete_files`). Media identity, user playback facts, collections and
//! subscriptions are always kept.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{LedgerId, LibraryId, MediaId};
use serde_json::json;
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;
use store::StoreError;

/// One file of the item being deleted.
struct ItemFile {
    file_id: LedgerId,
    path: String,
}

/// Bytes the path occupies right now (`0` when it is already gone).
fn observed_size(path: &str) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn store_error(error: StoreError) -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string())
}

/// The files this Library owns for the item, newest identity first.
fn item_files(
    store: &crate::Store,
    library_id: &str,
    media: &domain::Media,
) -> Result<Vec<ItemFile>, StoreError> {
    let rows = store.ledger_for_media(media.id)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            crate::http::library::library_for_row_strict(store, row, media)
                .is_some_and(|library| library.id == library_id)
        })
        .map(|row| ItemFile {
            file_id: row.id,
            path: row.path,
        })
        .collect())
}

pub(crate) async fn delete_item(
    State(state): State<ApiState>,
    Path((library_id, item_id)): Path<(String, String)>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let (files, removed) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&library_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        };
        let accepts_work = match store.library_accepts_work(&library_id) {
            Ok(accepts) => accepts,
            Err(error) => return store_error(error),
        };
        if !accepts_work {
            return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
        }
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.missing", "条目不存在");
        };
        let files = match item_files(&store, &library_id, &media) {
            Ok(files) => files,
            Err(error) => return store_error(error),
        };
        if files.is_empty() {
            return err(
                StatusCode::NOT_FOUND,
                "library.missing",
                "条目不属于该媒体库",
            );
        }
        // Physical bytes are removed only through the durable queue, so a crash
        // between the database and the filesystem still leaves a retry behind.
        for file in &files {
            if let Err(error) = store.enqueue_file_deletion(&file.path, "library_item_deleted") {
                return store_error(error);
            }
        }
        let library_id = match library.id.parse::<LibraryId>() {
            Ok(id) => id,
            Err(error) => return store_error(error.into()),
        };
        let file_ids: Vec<LedgerId> = files.iter().map(|file| file.file_id).collect();
        let removed = match store.delete_files(library_id, &file_ids) {
            Ok(removed) => removed,
            Err(error) => return store_error(error),
        };
        (files, removed)
    };

    let freed_bytes: u64 = files.iter().map(|file| observed_size(&file.path)).sum();
    let mut errors = Vec::new();
    if let Err(error) = crate::http::file_delete::retry_queued_deletions(&state.store) {
        tracing::error!(%error, "删除媒体库文件失败，保留在待删除队列等待重试");
        errors.push(format!("删除文件失败: {error}"));
    }
    let removed_paths: Vec<String> = files.into_iter().map(|file| file.path).collect();
    ok(json!({
        "removed_paths": removed_paths,
        "rows_deleted": removed,
        "freed_bytes": freed_bytes,
        "errors": errors,
    }))
    .into_response()
}
