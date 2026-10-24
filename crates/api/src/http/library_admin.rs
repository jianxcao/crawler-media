//! Library admin views: raw ledger rows and unidentified files.
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::MediaId;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;

/// GET /ledger — every ledger row (management view).
pub(crate) async fn list_ledger(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let rows_with_mode = match store.list_file_coverage() {
        Ok(rows) => rows,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let global_transfer_mode = match store.transfer_mode() {
        Ok(mode) => mode,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let mut items: Vec<Value> = Vec::new();
    let mut pending = Vec::with_capacity(rows_with_mode.len());
    for coverage in rows_with_mode {
        let file = coverage.file;
        let media = store.get_media(file.media_id).ok().flatten();
        let library_id = media.as_ref().and_then(|media| {
            crate::http::library::library_for_file(&store, &file, media).map(|library| library.id)
        });
        let title = media.as_ref().map(|m| m.title.clone()).unwrap_or_default();
        let kind = media.as_ref().map(|m| m.kind.as_str()).unwrap_or("movie");
        let is_strm = file
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));
        let path_json = file.path.to_string_lossy().into_owned();
        let source_path = file
            .source_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        pending.push((
            file.path.clone(),
            is_strm,
            file.transfer_mode.clone(),
            file.size_bytes,
        ));
        items.push(json!({
            "id": file.file_id.to_string(),
            "media_id": file.media_id.to_string(),
            "library_id": library_id,
            "media_title": title,
            "media_kind": kind,
            "path": path_json,
            "source_path": source_path,
            "units": coverage.units,
            "resolution": file.resolution,
            "codec": file.codec,
            "hdr": file.hdr,
            "quality_source": file.quality_source.as_str(),
            "confidence": file.confidence.as_str(),
            "filter_score": file.filter_score,
        }));
    }
    drop(store);
    // Disk observation (hardlink count, unknown sizes) runs after the store lock is released.
    for (item, (path, is_strm, saved_mode, size_bytes)) in items.iter_mut().zip(pending) {
        let meta = (!is_strm || size_bytes.is_none())
            .then(|| std::fs::metadata(&path).ok())
            .flatten();
        let file_size = size_bytes
            .or_else(|| meta.as_ref().map(|m| m.len()))
            .unwrap_or(0);
        item["transfer_mode"] = json!(transfer_mode_label(
            is_strm,
            meta.as_ref(),
            saved_mode,
            &global_transfer_mode
        ));
        item["file_size"] = json!(file_size);
    }
    ok_list(items).into_response()
}
fn transfer_mode_label(
    is_strm: bool,
    meta: Option<&std::fs::Metadata>,
    saved_mode: Option<String>,
    global_transfer_mode: &str,
) -> String {
    #[cfg(unix)]
    let is_hardlink = {
        use std::os::unix::fs::MetadataExt;
        meta.is_some_and(|m| m.nlink() > 1)
    };
    #[cfg(not(unix))]
    let is_hardlink = {
        let _ = meta;
        saved_mode.as_deref() == Some("hardlink") || global_transfer_mode == "hardlink"
    };
    if is_strm {
        "strm".to_string()
    } else if is_hardlink {
        "hardlink".to_string()
    } else {
        saved_mode.unwrap_or_else(|| global_transfer_mode.to_string())
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct DeleteLedgerQuery {
    delete_file: Option<bool>,
}

/// DELETE /ledger/{id} — delete a file row and optionally remove the bytes on disk.
pub(crate) async fn delete_ledger_row(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteLedgerQuery>,
) -> Response {
    let ledger_id = match domain::LedgerId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "ledger.invalid", "ledger id 无效"),
    };
    let delete_file = query.delete_file.unwrap_or(false);
    let target = {
        let store = state.store.lock();
        let target = match store.file_coverage_by_id(ledger_id) {
            Ok(Some(target)) => target,
            Ok(None) => return err(StatusCode::NOT_FOUND, "ledger.missing", "整理记录不存在"),
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        if let Some(media) = store.get_media(target.file.media_id).ok().flatten() {
            let owner = crate::http::library::library_for_file_strict(&store, &target.file, &media);
            match owner {
                Some(owner) => match store.library_accepts_work(&owner.id) {
                    Ok(true) => {}
                    Ok(false) => {
                        return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
                    }
                    Err(error) => {
                        return err(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "store.error",
                            &error.to_string(),
                        );
                    }
                },
                None if delete_file => {
                    return err(
                        StatusCode::FORBIDDEN,
                        "library.outside_root",
                        "文件已不在任何媒体库根目录内，拒绝物理删除",
                    );
                }
                None => {}
            }
        } else if delete_file {
            return err(
                StatusCode::FORBIDDEN,
                "library.outside_root",
                "文件已不在任何媒体库根目录内，拒绝物理删除",
            );
        }
        if delete_file
            && let Err(error) = store
                .enqueue_file_deletion(target.file.path.to_string_lossy().as_ref(), "admin_delete")
        {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        if let Err(error) = store.delete_ledger_path(target.file.path.to_string_lossy().as_ref()) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        target
    };
    // 物理删除只走 durable 队列：进程中断后仍会重试。
    if delete_file
        && let Err(error) = crate::http::file_delete::retry_queued_deletions(&state.store)
    {
        tracing::error!(path = %target.file.path.display(), %error, "管理删除物理文件失败，保留在待删除队列");
    }
    ok(json!({
        "deleted": true,
        "path": target.file.path.to_string_lossy().into_owned(),
        "file_deleted": delete_file
    }))
    .into_response()
}

/// GET /unidentified — files that failed confidence matching.
pub(crate) async fn list_unidentified(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let store = state.store.lock();
    let rows = match store.list_unidentified() {
        Ok(rows) => rows,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let target_lib_id = query.get("library_id").map(|s| s.as_str());
    let mut items: Vec<Value> = Vec::new();

    for (path, confidence) in rows {
        let fspath = std::path::Path::new(&path);
        let lib_match = store
            .library_for_path(fspath, domain::MediaKind::Movie)
            .ok()
            .flatten()
            .or_else(|| {
                store
                    .library_for_path(fspath, domain::MediaKind::Tv)
                    .ok()
                    .flatten()
            });
        let lib_id = lib_match.as_ref().map(|l| l.id.clone()).unwrap_or_default();
        let lib_name = lib_match
            .as_ref()
            .map(|l| l.name.clone())
            .unwrap_or_default();
        if let Some(target_id) = target_lib_id {
            if lib_id != target_id {
                continue;
            }
        }
        let file_name = fspath
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&path)
            .to_string();
        items.push(json!({
            "key": path.clone(),
            "path": path.clone(),
            "confidence": confidence,
            "label": file_name,
            "library_id": lib_id,
            "library_name": lib_name,
            "file_count": 1,
            "total_size_bytes": 0,
            "files": [{
                "file_id": path.clone(),
                "file_path": path,
            }],
        }));
    }
    drop(store);
    // Unidentified rows have no persisted size; stat them after releasing the store lock.
    for item in &mut items {
        let size = item["path"]
            .as_str()
            .and_then(|path| std::fs::metadata(path).ok())
            .map_or(0, |m| m.len());
        item["total_size_bytes"] = json!(size);
    }
    ok_list(items).into_response()
}

/// Resolve what a retransfer targets: the Library destination the coverage was
/// published to and the download source this round recorded for it. Failures
/// carry the HTTP response the handler returns, so the store read stays in one
/// place and the handler only drives the transfer.
fn retransfer_target(
    store: &crate::Store,
    ledger_id: domain::LedgerId,
) -> Result<(std::path::PathBuf, String), Response> {
    let rows_with_mode = match store.list_file_coverage() {
        Ok(rows) => rows,
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    };
    let Some(target) = rows_with_mode
        .into_iter()
        .find(|coverage| coverage.file.file_id == ledger_id)
    else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "ledger.missing",
            "整理记录不存在",
        ));
    };
    let Some(src) = target
        .file
        .source_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
    else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "ledger.no_source",
            "该条目未记录源文件地址，无法重新生成",
        ));
    };
    let Some(media) = store.get_media(target.file.media_id).ok().flatten() else {
        return Err(err(
            StatusCode::FORBIDDEN,
            "library.outside_root",
            "无法确认媒体库归属，拒绝重新生成",
        ));
    };
    if crate::http::library::library_for_file_strict(store, &target.file, &media).is_none() {
        return Err(err(
            StatusCode::FORBIDDEN,
            "library.outside_root",
            "目标已不在任何媒体库根目录内，拒绝重新生成",
        ));
    }
    Ok((target.file.path, src))
}

/// POST /ledger/{id}/retransfer — 从源地址重新硬链接/重新生成文件到媒体库
pub(crate) async fn retransfer_ledger_row(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let ledger_id = match domain::LedgerId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "ledger.invalid", "ledger id 无效"),
    };
    let (destination, src) = {
        let store = state.store.lock();
        match retransfer_target(&store, ledger_id) {
            Ok(target) => target,
            Err(response) => return response,
        }
    };
    let src_path = std::path::PathBuf::from(&src);
    let dest_path = std::path::PathBuf::from(&destination);
    if !src_path.is_file() {
        return err(
            StatusCode::BAD_REQUEST,
            "ledger.source_not_found",
            &format!("源下载文件已不存在: {src}"),
        );
    }
    if dest_path.exists() {
        return err(
            StatusCode::CONFLICT,
            "ledger.destination_exists",
            "媒体库目标文件仍存在，无需重新生成",
        );
    }
    let source_for_task = src_path.clone();
    let destination_for_task = dest_path.clone();
    let transfer = tokio::task::spawn_blocking(move || {
        library::transfer_file(
            &source_for_task,
            &destination_for_task,
            library::TransferMode::Hardlink,
        )
        .map_err(|error| error.to_string())
    })
    .await;
    let transfer = match transfer {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%error, "媒体库重新转移任务失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "transfer.failed",
                &error.to_string(),
            );
        }
    };
    if let Err(error) = transfer {
        tracing::error!(
            source = %src_path.display(),
            destination = %dest_path.display(),
            %error,
            "无法从其记录的源路径重新转移媒体库文件"
        );
        return err(StatusCode::INTERNAL_SERVER_ERROR, "transfer.failed", &error);
    }
    tracing::info!(
        source = %src_path.display(),
        destination = %dest_path.display(),
        "已从其记录的源路径重新转移媒体库文件"
    );

    if let Err(error) = state
        .store
        .lock()
        .clear_ledger_missing(destination.to_string_lossy().as_ref())
    {
        tracing::error!(
            destination = %destination.display(),
            %error,
            "重新转移后无法清除缺失标记"
        );
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }

    ok(json!({
        "retransferred": true,
        "source": src,
        "destination": destination.to_string_lossy().into_owned()
    }))
    .into_response()
}
/// Bind an unidentified file to an existing Media in place (no bytes move);
/// only a path strictly inside a Library root is accepted.
pub(crate) fn bind_existing(state: &ApiState, id: &str, path: &str) -> Response {
    let path = path.trim();
    if path.is_empty() {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "path 必填");
    }
    let media_id = match MediaId::from_str(id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "media id 无效"),
    };
    let Some(media) = state.store.lock().get_media(media_id).ok().flatten() else {
        return err(
            StatusCode::NOT_FOUND,
            "library.item_missing",
            "影视条目不存在",
        );
    };
    let facts = crate::claim::ClaimedFacts::for_path(media.kind, std::path::Path::new(path));
    let file = match crate::claim::publish_claimed_file(
        &state.store,
        &media,
        std::path::Path::new(path),
        &facts,
    ) {
        Ok(file) => file,
        Err(store::StoreError::Invalid(message)) => {
            return err(StatusCode::BAD_REQUEST, "library.path_outside", &message);
        }
        Err(store::StoreError::Protected(message)) => {
            return err(StatusCode::CONFLICT, "library.write_conflict", &message);
        }
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    if let Err(error) = state.store.lock().delete_unidentified(path) {
        tracing::warn!(path, %error, "Claim published but Unidentified cleanup failed");
    }
    crate::http::library::enqueue_probes_for_files(state, std::slice::from_ref(&file));
    ok(json!({
        "media_item_id": media.id.to_string(),
        "path": path,
    }))
    .into_response()
}
