//! Organize apply: durable renames under an explicit lock order.
//!
//! Lock order (S02): every Library work lease (source and named destinations)
//! is acquired up front and owned by this module's caller frame, never inside a
//! publication gate; a lease is likewise only dropped outside any gate. Each
//! entry publishes twice through the source gate — the intent (validate + begin)
//! and the ledger completion — with the Store taken only inside those short
//! publications. Disk IO (byte move, NFO) runs with no gate and no Store held.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::library_lifecycle::LibraryWorkLease;
use crate::management::ApiState;

use super::library::missing_library;

/// One validated-shape organize entry (paths only; ownership is checked at publish).
pub(crate) struct PreparedOrganizeEntry {
    pub(crate) from: PathBuf,
    pub(crate) to: PathBuf,
    pub(crate) destination_library_id: String,
}

enum Prepared {
    Begun(store::library_write::FileRenameRequest),
    Skipped(String),
    Failed(String),
}

pub(crate) fn organize_inner(state: &ApiState, id: &str, body: &Value) -> Response {
    let source_lease = match state.acquire_library_work(id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.frozen", &error),
    };
    let Some(renames) = body["renames"].as_array() else {
        return err(
            StatusCode::BAD_REQUEST,
            "organize.invalid",
            "缺少 renames 列表",
        );
    };
    let Some(library) = state.store.lock().get_library(id).ok().flatten() else {
        return missing_library();
    };
    let entries: Vec<PreparedOrganizeEntry> = renames
        .iter()
        .filter_map(|entry| {
            let (from, to) = (entry["from"].as_str()?, entry["to"].as_str()?);
            let destination = entry["to_library_id"].as_str().unwrap_or(id);
            Some(PreparedOrganizeEntry {
                from: from.into(),
                to: to.into(),
                destination_library_id: destination.to_string(),
            })
        })
        .collect();
    // All destination leases before any publication; owned here, dropped after.
    let mut destinations: HashMap<String, Result<LibraryWorkLease, String>> = HashMap::new();
    for entry in &entries {
        if entry.destination_library_id != library.id
            && !destinations.contains_key(&entry.destination_library_id)
        {
            let lease = state.acquire_library_work(&entry.destination_library_id);
            destinations.insert(entry.destination_library_id.clone(), lease);
        }
    }
    let response = apply_organize(state, &library, &source_lease, &destinations, &entries);
    drop(destinations);
    drop(source_lease);
    response
}

pub(crate) fn apply_organize(
    state: &ApiState,
    library: &store::Library,
    source_lease: &LibraryWorkLease,
    destinations: &HashMap<String, Result<LibraryWorkLease, String>>,
    entries: &[PreparedOrganizeEntry],
) -> Response {
    let mut applied = 0usize;
    let mut skipped = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let (from, to) = (
            entry.from.display().to_string(),
            entry.to.display().to_string(),
        );
        if entry.to == entry.from {
            applied += 1;
            continue;
        }
        let dest = destinations.get(&entry.destination_library_id);
        if let Some(Err(error)) = dest {
            errors.push(format!("{to}: 目标媒体库不可写: {error}"));
            continue;
        }
        let dest_lease = dest.and_then(|d| d.as_ref().ok());
        let now_ms = crate::job_loop::unix_now();
        let prepared = source_lease.publish(|| {
            if dest_lease.is_some_and(|d| d.control().is_cancelled()) {
                return Err("目标媒体库正在删除".into());
            }
            Ok(prepare_rename(state, library, entry, now_ms))
        });
        let request = match prepared {
            Ok(Prepared::Begun(request)) => request,
            Ok(Prepared::Skipped(reason)) => {
                skipped.push(reason);
                continue;
            }
            Ok(Prepared::Failed(reason)) => {
                errors.push(reason);
                continue;
            }
            Err(error) => {
                errors.push(format!("{from}: 媒体库工作已取消: {error}"));
                continue;
            }
        };
        // No gate, no Store: the durable intent is already committed.
        if let Err(error) = store::library_write::move_rename_bytes(&request) {
            tracing::error!(from, to, %error, "整理文件重命名失败");
            errors.push(format!("{from} 重命名失败: {error}"));
            continue;
        }
        let completed = source_lease.publish(|| {
            if dest_lease.is_some_and(|d| d.control().is_cancelled()) {
                return Err("目标媒体库正在删除".into());
            }
            let store = state.store.lock();
            store
                .apply_file_rename_disk(&request.rename_id, now_ms)
                .and_then(|_| store.complete_file_rename(&request.rename_id, now_ms))
                .map_err(|error| error.to_string())?;
            Ok(store
                .file_coverage_by_path(&to)
                .ok()
                .flatten()
                .and_then(|c| store.get_media(c.file.media_id).ok().flatten()))
        });
        let media = match completed {
            Ok(media) => media,
            Err(error) => {
                // The intent stays pending; startup recovery finishes it.
                tracing::error!(from, to, %error, rename_id = %request.rename_id, "整理文件更新台账失败");
                errors.push(format!("{from} -> {to}: 台账记录更新失败: {error}"));
                continue;
            }
        };
        if let Some(media) = media {
            write_missing_nfo(&entry.to, &media);
        }
        applied += 1;
    }
    ok(json!({ "applied": applied, "skipped": skipped, "errors": errors })).into_response()
}

fn write_missing_nfo(to: &FsPath, media: &domain::Media) {
    let Some(stem) = to.file_stem().and_then(|n| n.to_str()) else {
        return;
    };
    let nfo_path = to.with_file_name(format!("{stem}.nfo"));
    if !nfo_path.is_file()
        && let Err(error) = library::write_nfo(&nfo_path, media, None)
    {
        tracing::warn!(path = %nfo_path.display(), %error, "整理后写入 NFO 失败");
    }
}

/// Ownership checks and durable intent; runs inside the source publication.
fn prepare_rename(
    state: &ApiState,
    library: &store::Library,
    entry: &PreparedOrganizeEntry,
    now_ms: i64,
) -> Prepared {
    let (from_path, to_path) = (entry.from.as_path(), entry.to.as_path());
    let (from, to) = (
        from_path.display().to_string(),
        to_path.display().to_string(),
    );
    let store = state.store.lock();
    let owner = |path: &FsPath| {
        store
            .library_for_path_strict(path, library.kind)
            .ok()
            .flatten()
            .map(|l| l.id)
    };
    if owner(from_path).as_deref() != Some(library.id.as_str()) {
        return Prepared::Failed(format!("{from}: 源文件不属于当前媒体库"));
    }
    let expected_dest = entry.destination_library_id.as_str();
    if owner(to_path).as_deref() != Some(expected_dest) {
        tracing::warn!(
            from,
            to,
            expected_dest,
            "整理目标不属于指定的媒体库，拒绝改挂"
        );
        return Prepared::Failed(format!("{to}: 目标路径不属于当前媒体库范围"));
    }
    if !from_path.is_file() {
        return Prepared::Skipped(format!("源文件不存在 {from}"));
    }
    if to_path.exists() {
        return Prepared::Skipped(format!("目标已存在 {to}"));
    }
    if store.file_coverage_by_path(&to).ok().flatten().is_some() {
        return Prepared::Failed(format!("{to}: 目标路径已在台账中存在"));
    }
    let Some(source) = store.file_coverage_by_path(&from).ok().flatten() else {
        return Prepared::Failed(format!("{from}: 源文件未登记在台账中"));
    };
    let Some(media) = store.get_media(source.file.media_id).ok().flatten() else {
        return Prepared::Failed(format!("{from}: 关联媒体信息不存在"));
    };
    if crate::http::library::library_for_file(&store, &source.file, &media)
        .as_ref()
        .map(|l| &l.id)
        != Some(&library.id)
    {
        return Prepared::Failed(format!("{from}: 台账记录不属于当前媒体库"));
    }
    let (file_id, version) = (source.file.file_id, source.file.version.clone());
    match store.begin_file_rename(file_id, &version, from_path, to_path, now_ms) {
        Ok(request) => Prepared::Begun(request),
        Err(error) => {
            tracing::warn!(from, to, %error, "整理目标未通过归属复核，拒绝改名");
            Prepared::Failed(format!("{from} -> {to}: {error}"))
        }
    }
}
