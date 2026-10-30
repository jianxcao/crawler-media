//! Duplicate files detection and deletion.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::path::Path as FsPath;

use crate::http::{err, ok};
use crate::management::ApiState;

use super::library::missing_library;

mod listing;
pub(crate) use listing::duplicates;

struct FileObservation {
    entity: Option<(u64, u64)>,
    size: i64,
}

fn observe_file(files: &dyn crate::file_stat_read::FileStatRead, path: &FsPath) -> FileObservation {
    let metadata = match files.metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::debug!(path=%path.display(), %error, "重复文件观察不可用");
            return FileObservation {
                entity: None,
                size: 0,
            };
        }
    };
    #[cfg(unix)]
    let entity = {
        use std::os::unix::fs::MetadataExt;
        metadata.is_file().then(|| (metadata.dev(), metadata.ino()))
    };
    #[cfg(not(unix))]
    let entity = {
        use std::hash::{Hash, Hasher};
        std::fs::canonicalize(path)
            .ok()
            .filter(|_| metadata.is_file())
            .map(|canonical| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                canonical.hash(&mut hasher);
                (0, hasher.finish())
            })
    };
    FileObservation {
        entity,
        size: metadata.len().min(i64::MAX as u64) as i64,
    }
}

/// DELETE /libraries/{id}/duplicates/{file_id} — physically delete one duplicate file.
///
/// Every unit the target covers must remain covered by a different, present,
/// published physical entity in the same Library (set containment, not "any
/// overlap"). Filesystem identity is read without the Store lock; the plan is
/// then revalidated against the captured file versions, a durable deletion
/// intent is queued, and the bytes are removed off-lock.
pub(crate) async fn delete_duplicate(
    State(state): State<ApiState>,
    Path((library_id, file_id)): Path<(String, String)>,
) -> Response {
    let Ok(file_id) = file_id.parse::<domain::LedgerId>() else {
        return err(StatusCode::NOT_FOUND, "file.missing", "文件不存在");
    };
    let worker = state.clone();
    match state
        .fs_work
        .run_to_completion(move || delete_duplicate_blocking(&worker, &library_id, file_id))
        .await
    {
        Ok(response) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

fn delete_duplicate_blocking(
    state: &ApiState,
    library_id: &str,
    file_id: domain::LedgerId,
) -> Response {
    let plan = match capture_plan(state, library_id, file_id) {
        Ok(plan) => plan,
        Err(response) => return *response,
    };
    let observed: Vec<_> = plan
        .files
        .iter()
        .map(|file| observe_file(state.file_stats.as_ref(), &file.file.path).entity)
        .collect();
    // Capture the durable lstat identity outside Store, before accepting the intent.
    let path = plan.files[0].file.path.to_string_lossy().into_owned();
    let identity = store::file_identity(&path);
    if let Err(response) = prove_redundant(&plan, &observed) {
        return *response;
    }
    let queued = super::store_wait::with_foreground_store(state, |store| {
        store.library_write_with_error(
            |error| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "file.delete",
                    &error.to_string(),
                )
            },
            |store| {
                match capture_plan_on(store, library_id, file_id) {
                    Ok(again) if again.versions() == plan.versions() => {}
                    Ok(_) => return Err(not_duplicate("文件在检查期间已变化，拒绝删除")),
                    Err(response) => return Err(*response),
                }
                store
                    .enqueue_file_deletion_observed(&path, "duplicate", identity)
                    .map_err(|error| {
                        err(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "file.delete",
                            &error.to_string(),
                        )
                    })
            },
        )
    });
    match queued {
        Ok(Ok(())) => {}
        Ok(Err(response)) | Err(response) => return response,
    }
    if let Err(error) = crate::http::file_delete::retry_queued_deletions_for(&state.store, &path) {
        tracing::error!(%error, path = %path, "删除重复文件失败，保留删除意图稍后重试");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "file.delete", &error);
    }
    tracing::info!(path = %path, file_id = %file_id, "已删除重复文件");
    ok(json!({ "deleted": true, "path": path })).into_response()
}

/// Target first, then every other file of the same Media in the Library.
struct DeletePlan {
    files: Vec<domain::FileCoverage>,
}

impl DeletePlan {
    fn versions(&self) -> Vec<(domain::LedgerId, domain::FileVersion, Vec<domain::UnitKey>)> {
        let mut out: Vec<_> = self
            .files
            .iter()
            .map(|c| (c.file.file_id, c.file.version.clone(), c.units.clone()))
            .collect();
        out[1..].sort_by_key(|entry| entry.0.to_string());
        out
    }
}

fn not_duplicate(message: &str) -> Response {
    err(StatusCode::BAD_REQUEST, "file.not_duplicate", message)
}

fn capture_plan(
    state: &ApiState,
    library_id: &str,
    file_id: domain::LedgerId,
) -> Result<DeletePlan, Box<Response>> {
    super::store_wait::with_store_read(state, |store| {
        capture_plan_locked(store, library_id, file_id)
    })
    .map_err(Box::new)?
}

fn capture_plan_locked(
    store: &crate::Store,
    library_id: &str,
    file_id: domain::LedgerId,
) -> Result<DeletePlan, Box<Response>> {
    let plan = store
        .library_read_snapshot(|store| capture_plan_on(store, library_id, file_id))
        .map_err(|error| {
            Box::new(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ))
        })??;
    let target = &plan.files[0];
    let media = store
        .get_media(target.file.media_id)
        .ok()
        .flatten()
        .ok_or_else(|| Box::new(not_duplicate("Media 已不存在")))?;
    if super::library::library_for_file_strict(store, &target.file, &media).is_none() {
        return Err(Box::new(not_duplicate("文件不属于该媒体库")));
    }
    Ok(plan)
}

fn capture_plan_on(
    store: &store::LibraryConnection<'_>,
    library_id: &str,
    file_id: domain::LedgerId,
) -> Result<DeletePlan, Box<Response>> {
    let missing = || Box::new(err(StatusCode::NOT_FOUND, "file.missing", "文件不存在"));
    let Some(library) = store.get_library(library_id).ok().flatten() else {
        return Err(Box::new(missing_library()));
    };
    let target = store
        .file_coverage_by_id(file_id)
        .ok()
        .flatten()
        .ok_or_else(missing)?;
    let media = store
        .get_media(target.file.media_id)
        .ok()
        .flatten()
        .ok_or_else(missing)?;
    if crate::http::library::library_for_recorded_file(store, &target, &media)
        .is_none_or(|owner| owner.id != library.id)
    {
        return Err(Box::new(err(
            StatusCode::NOT_FOUND,
            "file.missing",
            "文件不属于该媒体库",
        )));
    }
    if !store.library_accepts_work(library_id).map_err(|error| {
        Box::new(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ))
    })? {
        return Err(Box::new(err(
            StatusCode::CONFLICT,
            "library.deleting",
            "Library 正在删除",
        )));
    }
    let mut files = vec![target];
    let id = files[0].file.library_id;
    files.extend(
        store
            .file_coverage_in_library(id, Some(media.id), store::FileReadScope::AllRecorded)
            .map_err(|error| {
                Box::new(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                ))
            })?
            .into_iter()
            .filter(|other| other.file.file_id != file_id),
    );
    Ok(DeletePlan { files })
}

fn prove_redundant(
    plan: &DeletePlan,
    observed: &[Option<(u64, u64)>],
) -> Result<(), Box<Response>> {
    let refuse = || Box::new(not_duplicate("没有另一份在位的独立副本，拒绝删除"));
    let target = &plan.files[0];
    if target.units.is_empty() {
        return Err(refuse());
    }
    let target_entity = observed[0];
    for unit in &target.units {
        let covered = plan
            .files
            .iter()
            .zip(observed)
            .skip(1)
            .any(|(other, entity)| {
                other.file.publication == domain::FilePublication::Published
                    && other.units.contains(unit)
                    && entity.is_some()
                    && *entity != target_entity
            });
        if !covered {
            return Err(refuse());
        }
    }
    Ok(())
}
