//! Duplicate files detection and deletion.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path as FsPath;

use crate::http::{err, file_delete, ok, ok_list};
use crate::management::ApiState;

use super::library::missing_library;

/// Physical identity of a present file: hard links share one entity, so a
/// second path to the same inode is never an independent copy.
#[cfg(unix)]
fn file_entity(path: &FsPath) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    meta.is_file().then(|| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_entity(path: &FsPath) -> Option<(u64, u64)> {
    let canonical = std::fs::canonicalize(path).ok()?;
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    Some((0, hasher.finish()))
}

/// Only recorded coverage establishes duplicate logical units. Unidentified
/// files must not become interchangeable whole-work copies.
fn unit_coordinates(units: &[domain::UnitKey]) -> Vec<(Option<u32>, Option<u32>)> {
    units
        .iter()
        .map(|unit| {
            if unit.is_whole() {
                (None, None)
            } else {
                (Some(unit.season as u32), Some(unit.episode as u32))
            }
        })
        .collect()
}

/// Units of a file in the library listing, recorded or not.
fn units_of(store: &crate::Store, file: &domain::LibraryFile) -> Vec<domain::UnitKey> {
    match store.get_file_units(file.file_id) {
        Ok(units) => units,
        Err(error) => {
            tracing::warn!(%error, file_id = %file.file_id, "读取文件单元失败");
            Vec::new()
        }
    }
}

fn distinct_present_entities(rows: &[domain::LibraryFile]) -> Vec<(u64, u64)> {
    let mut entities = Vec::new();
    for row in rows {
        let Some(entity) = file_entity(&row.path) else {
            continue;
        };
        if !entities.iter().any(|existing| existing == &entity) {
            entities.push(entity);
        }
    }
    entities
}

/// GET /libraries/{id}/duplicates — file rows sharing the same
/// (media, season, episode) unit, grouped for keep/delete decisions.
pub(crate) async fn duplicates(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let store = state.store.lock();
    let Some(library) = store.get_library(&id).ok().flatten() else {
        return missing_library();
    };
    let mut units: HashMap<(domain::MediaId, Option<u32>, Option<u32>), Vec<domain::LibraryFile>> =
        HashMap::new();
    for row in super::library::files_in_library(&store, &library) {
        for (season, episode) in unit_coordinates(&units_of(&store, &row)) {
            units
                .entry((row.media_id, season, episode))
                .or_default()
                .push(row.clone());
        }
    }
    let mut groups = Vec::new();
    for ((media_id, season, episode), rows) in units {
        if distinct_present_entities(&rows).len() < 2 {
            continue;
        }
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            continue;
        };
        let files: Vec<Value> = rows
            .iter()
            .map(|row| {
                json!({
                    "file_id": row.file_id.to_string(),
                    "path": row.path.display().to_string(),
                    "resolution": row.resolution,
                    "codec": row.codec,
                    "hdr": row.hdr,
                    "quality_source": row.quality_source.as_str(),
                    "size_bytes": std::fs::metadata(&row.path).map(|m| m.len() as i64).unwrap_or(0),
                })
            })
            .collect();
        groups.push(json!({
            "media_item_id": media.id.to_string(),
            "title": media.title,
            "season": season,
            "episode": episode,
            "files": files,
        }));
    }
    ok_list(groups).into_response()
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
    let plan = match capture_plan(&state, &library_id, file_id) {
        Ok(plan) => plan,
        Err(response) => return *response,
    };
    let observed = {
        let paths: Vec<_> = plan.files.iter().map(|f| f.file.path.clone()).collect();
        match tokio::task::spawn_blocking(move || {
            paths.iter().map(|p| file_entity(p)).collect::<Vec<_>>()
        })
        .await
        {
            Ok(entities) => entities,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "file.delete",
                    &error.to_string(),
                );
            }
        }
    };
    if let Err(response) = prove_redundant(&plan, &observed) {
        return *response;
    }
    let path = plan.files[0].file.path.to_string_lossy().into_owned();
    {
        let store = state.store.lock();
        match capture_plan_locked(&store, &library_id, file_id) {
            Ok(again) if again.versions() == plan.versions() => {}
            Ok(_) => return not_duplicate("文件在检查期间已变化，拒绝删除"),
            Err(response) => return *response,
        }
        if let Err(error) = store.enqueue_file_deletion(&path, "duplicate") {
            tracing::error!(%error, path = %path, "登记重复文件删除意图失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "file.delete",
                &error.to_string(),
            );
        }
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
    let store = state.store.lock();
    capture_plan_locked(&store, library_id, file_id)
}

fn capture_plan_locked(
    store: &crate::Store,
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
    if crate::http::library::library_for_file_strict(store, &target.file, &media)
        .is_none_or(|owner| owner.id != library.id)
    {
        return Err(Box::new(err(
            StatusCode::NOT_FOUND,
            "file.missing",
            "文件不属于该媒体库",
        )));
    }
    let mut files = vec![target];
    files.extend(
        super::library::coverages_in_library(store, &library)
            .into_iter()
            .filter(|other| other.file.media_id == media.id && other.file.file_id != file_id),
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
