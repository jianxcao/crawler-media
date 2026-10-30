//! Library admin views: raw ledger rows and unidentified files.
use axum::body::HttpBody;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Instant;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;

/// GET /ledger — every ledger row (management view).
pub(crate) async fn list_ledger(State(state): State<ApiState>) -> Response {
    let (mut items, pending, global_transfer_mode) =
        match super::store_wait::store_read(&state, capture_ledger).await {
            Ok(Ok(facts)) => facts,
            Ok(Err(response)) => return response,
            Err(error) => return error.response(),
        };
    let files = state.file_stats.clone();
    match super::fs::read_work(&state, move || {
        let rows = items.len();
        let observe_started = Instant::now();
        // Disk observation (hardlink count, unknown sizes) runs after the store lock is released.
        for (item, (path, is_strm, saved_mode, size_bytes)) in items.iter_mut().zip(pending) {
            let meta = (!is_strm || size_bytes.is_none())
                .then(|| files.metadata(&path).ok())
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
        let observe_ms = observe_started.elapsed().as_secs_f64() * 1000.;
        let serialize_started = Instant::now();
        let response = ok_list(items).into_response();
        let serialize_ms = serialize_started.elapsed().as_secs_f64() * 1000.;
        let body_bytes = response.body().size_hint().exact();
        tracing::debug!(
            rows,
            observe_ms,
            serialize_ms,
            body_bytes,
            "ledger response work completed"
        );
        response
    })
    .await
    {
        Ok(response) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

type LedgerFileObservation = (std::path::PathBuf, bool, Option<String>, Option<u64>);

fn capture_ledger(
    store: &crate::Store,
) -> Result<(Vec<Value>, Vec<LedgerFileObservation>, String), Response> {
    let sql_started = Instant::now();
    let rows_with_mode = match store.ledger_rows() {
        Ok(rows) => rows,
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    };
    let sql_ms = sql_started.elapsed().as_secs_f64() * 1000.;
    let rows = rows_with_mode.len();
    tracing::debug!(rows, sql_ms, "ledger fact capture completed");
    let global_transfer_mode = match store.transfer_mode() {
        Ok(mode) => mode,
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    };
    let assembly_started = Instant::now();
    let mut items: Vec<Value> = Vec::with_capacity(rows);
    let mut pending = Vec::with_capacity(rows_with_mode.len());
    for row in rows_with_mode {
        let coverage = row.coverage;
        let file = coverage.file;
        let library_id = file.library_id.to_string();
        let title = row.media_title;
        let kind = row.media_kind.as_str();
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
    let assembly_ms = assembly_started.elapsed().as_secs_f64() * 1000.;
    tracing::debug!(rows, assembly_ms, "ledger JSON values assembled");
    Ok((items, pending, global_transfer_mode))
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
    let target = match super::store_wait::store_read(&state, move |store| {
        ledger_delete_target(store, ledger_id, delete_file)
    })
    .await
    {
        Ok(Ok(target)) => target,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let path = target.file.path.to_string_lossy().into_owned();
    if delete_file {
        match super::file_delete::delete_coverages(&state, vec![target], "admin_delete").await {
            Ok(report) => {
                if !report.errors.is_empty() {
                    tracing::error!(%path, errors=?report.errors, "管理删除物理文件未完成，保留持久意图");
                    return err(
                        StatusCode::CONFLICT,
                        "file.delete_pending",
                        &format!(
                            "删除尚未完成，已保留持久意图供重试：{}",
                            report.errors.join("；")
                        ),
                    );
                }
            }
            Err(response) => return response,
        }
    } else {
        let result = super::store_wait::store_write(&state, move |store| {
            let current = ledger_delete_target(store, ledger_id, false)?;
            if !super::file_delete::same_recorded_source(&target, &current) {
                return Err(err(
                    StatusCode::CONFLICT,
                    "ledger.changed",
                    "文件版本或归属已变化",
                ));
            }
            store
                .delete_ledger_path(&current.file.path.to_string_lossy())
                .map_err(|error| {
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &error.to_string(),
                    )
                })
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(response)) | Err(response) => return response,
        }
    }
    ok(json!({ "deleted":true, "path":path, "file_deleted":delete_file })).into_response()
}

fn ledger_delete_target(
    store: &crate::Store,
    ledger_id: domain::LedgerId,
    delete_file: bool,
) -> Result<domain::FileCoverage, Response> {
    let target = match store.file_coverage_by_id(ledger_id) {
        Ok(Some(target)) => target,
        Ok(None) => {
            return Err(err(
                StatusCode::NOT_FOUND,
                "ledger.missing",
                "整理记录不存在",
            ));
        }
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    };
    if let Some(media) = store.get_media(target.file.media_id).ok().flatten() {
        let owner = crate::http::library::library_for_file_strict(store, &target.file, &media);
        match owner {
            Some(owner) => match store.library_accepts_work(&owner.id) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(err(
                        StatusCode::CONFLICT,
                        "library.deleting",
                        "Library 正在删除",
                    ));
                }
                Err(error) => {
                    return Err(err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &error.to_string(),
                    ));
                }
            },
            None if delete_file => {
                return Err(err(
                    StatusCode::FORBIDDEN,
                    "library.outside_root",
                    "文件已不在任何媒体库根目录内，拒绝物理删除",
                ));
            }
            None => {}
        }
    } else if delete_file {
        return Err(err(
            StatusCode::FORBIDDEN,
            "library.outside_root",
            "文件已不在任何媒体库根目录内，拒绝物理删除",
        ));
    }
    Ok(target)
}

/// GET /unidentified — files that failed confidence matching.
pub(crate) async fn list_unidentified(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let library_id = query.get("library_id").cloned();
    let mut items = match super::store_wait::store_read(&state, move |store| {
        capture_unidentified(store, library_id.as_deref())
    })
    .await
    {
        Ok(Ok(items)) => items,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let files = state.file_stats.clone();
    match super::fs::read_work(&state, move || {
        // Unidentified rows have no persisted size; stat them after releasing the store lock.
        for item in &mut items {
            let size = item["path"]
                .as_str()
                .and_then(|path| files.metadata(std::path::Path::new(path)).ok())
                .map_or(0, |m| m.len());
            item["total_size_bytes"] = json!(size);
        }
        ok_list(items).into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

fn capture_unidentified(
    store: &crate::Store,
    library_id: Option<&str>,
) -> Result<Vec<Value>, Response> {
    let rows = match store.list_unidentified(library_id) {
        Ok(rows) => rows,
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ));
        }
    };
    let items: Vec<Value> = rows.into_iter().map(|row| {
        let file_name = std::path::Path::new(&row.path).file_name()
            .and_then(|name| name.to_str()).unwrap_or(&row.path).to_string();
        json!({
            "key": row.unidentified_id, "unidentified_id":row.unidentified_id, "path": row.path, "confidence": row.confidence,
            "label": file_name, "library_id": row.library_id, "root_id": row.root_id,
            "library_name": row.library_name, "revision": row.revision,
            "file_count": 1, "total_size_bytes": 0,
            "files": [{ "file_id": row.unidentified_id, "file_path": row.path }],
        })
    }).collect();
    Ok(items)
}

/// Resolve what a retransfer targets: the Library destination the coverage was
/// published to and the download source this round recorded for it. Failures
/// carry the HTTP response the handler returns, so the store read stays in one
/// place and the handler only drives the transfer.
struct RetransferTarget {
    coverage: domain::FileCoverage,
    source: std::path::PathBuf,
    config_revision: i64,
}

fn retransfer_target(
    store: &crate::Store,
    ledger_id: domain::LedgerId,
) -> Result<RetransferTarget, Response> {
    let target = store
        .library_read_snapshot(|store| retransfer_target_on(store, ledger_id))
        .map_err(admin_store_error)??;
    let media = store
        .get_media(target.coverage.file.media_id)
        .map_err(admin_store_error)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "ledger.missing", "Media 已不存在"))?;
    if super::library::library_for_file_strict(store, &target.coverage.file, &media).is_none() {
        return Err(err(
            StatusCode::FORBIDDEN,
            "library.outside_root",
            "无法确认媒体库归属",
        ));
    }
    Ok(target)
}

fn retransfer_target_on(
    store: &store::LibraryConnection<'_>,
    ledger_id: domain::LedgerId,
) -> Result<RetransferTarget, Response> {
    let target = store
        .file_coverage_by_id(ledger_id)
        .map_err(admin_store_error)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "ledger.missing", "整理记录不存在"))?;
    let source = target.file.source_path.clone().ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "ledger.no_source",
            "该条目未记录源文件地址，无法重新生成",
        )
    })?;
    let media = store
        .get_media(target.file.media_id)
        .map_err(admin_store_error)?
        .ok_or_else(|| {
            err(
                StatusCode::FORBIDDEN,
                "library.outside_root",
                "无法确认媒体库归属，拒绝重新生成",
            )
        })?;
    if super::library::library_for_recorded_file(store, &target, &media).is_none() {
        return Err(err(
            StatusCode::FORBIDDEN,
            "library.outside_root",
            "目标已不在任何媒体库根目录内，拒绝重新生成",
        ));
    }
    let library = target.file.library_id.to_string();
    if store
        .file_deletion_pending(&target.file.path.to_string_lossy())
        .map_err(admin_store_error)?
    {
        return Err(err(
            StatusCode::CONFLICT,
            "ledger.deleting",
            "该文件由删除意图持有，拒绝重新生成",
        ));
    }
    let config_revision = store
        .library_config_revision(&library)
        .map_err(admin_store_error)?;
    Ok(RetransferTarget {
        coverage: target,
        source,
        config_revision,
    })
}

fn admin_store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "Library 管理事实操作失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

/// POST /ledger/{id}/retransfer — restore bytes using the captured current identity.
pub(crate) async fn retransfer_ledger_row(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let ledger_id = match domain::LedgerId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "ledger.invalid", "ledger id 无效"),
    };
    let worker = state.clone();
    match state
        .fs_work
        .run_to_completion(move || retransfer_blocking(&worker, ledger_id))
        .await
    {
        Ok(response) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

fn retransfer_blocking(state: &ApiState, ledger_id: domain::LedgerId) -> Response {
    let target = match super::store_wait::with_foreground_store(state, |store| {
        retransfer_target(store, ledger_id)
    }) {
        Ok(Ok(target)) => target,
        Ok(Err(response)) => return response,
        Err(response) => return response,
    };
    let claim = super::file_delete::path_claim(&target.coverage.path_key);
    let _claim = claim.lock();
    let lease = match state
        .library_work
        .acquire_bounded(&state.store, &target.coverage.file.library_id.to_string())
    {
        Ok(lease) => lease,
        Err(
            crate::library_lifecycle::BoundedLeaseError::StoreBusy
            | crate::library_lifecycle::BoundedLeaseError::RegistryBusy,
        ) => return super::store_wait::busy_response(),
        Err(message) => {
            return err(
                StatusCode::CONFLICT,
                "library.deleting",
                &message.to_string(),
            );
        }
    };
    let admitted = super::store_wait::with_foreground_store(state, |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        let current = retransfer_target(store, ledger_id)?;
        if !super::file_delete::same_recorded_source(&target.coverage, &current.coverage)
            || current.config_revision != target.config_revision
        {
            return Err(err(
                StatusCode::CONFLICT,
                "ledger.changed",
                "文件归属或版本已变化",
            ));
        }
        Ok(())
    });
    match admitted {
        Ok(Ok(())) => {}
        Ok(Err(response)) => return response,
        Err(response) => return response,
    }
    if !state.file_stats.is_file(&target.source) {
        return err(
            StatusCode::BAD_REQUEST,
            "ledger.source_not_found",
            "源下载文件已不存在",
        );
    }
    if target.coverage.file.path.exists() {
        return err(
            StatusCode::CONFLICT,
            "ledger.destination_exists",
            "媒体库目标文件仍存在，无需重新生成",
        );
    }
    if let Err(error) = lease.publish(|| {
        library::transfer_file(
            &target.source,
            &target.coverage.file.path,
            library::TransferMode::Hardlink,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }) {
        tracing::error!(%error, source=%target.source.display(), "重新转移 Library 文件失败");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "transfer.failed", &error);
    }
    let observed = match library::observe_source(&target.coverage.file.path) {
        Ok(observed) => observed,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "transfer.failed",
                &error.to_string(),
            );
        }
    };
    if let Err(response) = publish_retransfer(state, &target, &observed) {
        return response;
    }
    tracing::info!(file_id=%ledger_id, "Library 文件重新转移完成");
    ok(
        json!({ "retransferred":true, "source":target.source.to_string_lossy(),
        "destination":target.coverage.file.path.to_string_lossy() }),
    )
    .into_response()
}

fn publish_retransfer(
    state: &ApiState,
    target: &RetransferTarget,
    observed: &library::SourceObservation,
) -> Result<(), Response> {
    super::store_wait::with_foreground_store(state, |store| {
        store.library_write_with_error(admin_store_error, |store| {
            let ledger_id = target.coverage.file.file_id;
            let current = retransfer_target_on(store, ledger_id)?;
            if !super::file_delete::same_recorded_source(&target.coverage, &current.coverage) {
                return Err(err(
                    StatusCode::CONFLICT,
                    "ledger.changed",
                    "文件归属或版本已变化",
                ));
            }
            if observed.source_version != target.coverage.file.version.source_version {
                store
                    .replace_file_source(
                        &store::library_write::WriteScope {
                            library_id: target.coverage.file.library_id,
                            config_revision: target.config_revision,
                        },
                        ledger_id,
                        &target.coverage.file.version,
                        observed,
                        crate::job_loop::unix_now() * 1000,
                    )
                    .map_err(admin_store_error)?;
            }
            store
                .clear_ledger_missing(&target.coverage.file.path.to_string_lossy())
                .map_err(admin_store_error)?;
            Ok(())
        })
    })?
}
