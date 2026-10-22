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

fn file_entity(path: &FsPath) -> Option<std::path::PathBuf> {
    if !path.exists() {
        return None;
    }
    std::fs::canonicalize(path).ok()
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

fn distinct_present_entities(rows: &[domain::LibraryFile]) -> Vec<std::path::PathBuf> {
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
pub(crate) async fn delete_duplicate(
    State(state): State<ApiState>,
    Path((library_id, file_id)): Path<(String, String)>,
) -> Response {
    let store = state.store.lock();
    let Some(library) = store.get_library(&library_id).ok().flatten() else {
        return missing_library();
    };
    let Ok(file_id) = file_id.parse::<domain::LedgerId>() else {
        return err(StatusCode::NOT_FOUND, "file.missing", "文件不存在");
    };
    let Some(coverage) = store.file_coverage_by_id(file_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "file.missing", "文件不存在");
    };
    let row = coverage.file;
    let target_units = unit_coordinates(&coverage.units);
    let Some(media) = store.get_media(row.media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "file.missing", "文件不存在");
    };
    if crate::http::library::library_for_file_strict(&store, &row, &media)
        .is_none_or(|owner| owner.id != library.id)
    {
        return err(StatusCode::NOT_FOUND, "file.missing", "文件不属于该媒体库");
    }
    let siblings: Vec<domain::LibraryFile> = super::library::files_in_library(&store, &library)
        .into_iter()
        .filter(|other| {
            other.media_id == row.media_id
                && unit_coordinates(&units_of(&store, other))
                    .iter()
                    .any(|unit| target_units.contains(unit))
        })
        .collect();
    let present_entities = distinct_present_entities(&siblings);
    if present_entities.len() < 2 {
        return err(
            StatusCode::BAD_REQUEST,
            "file.not_duplicate",
            "没有另一份在位的独立副本，拒绝删除",
        );
    }
    let target_entity = file_entity(&row.path);
    let remaining_entities = present_entities
        .into_iter()
        .filter(|entity| Some(entity.as_path()) != target_entity.as_deref())
        .count();
    if remaining_entities == 0 {
        return err(
            StatusCode::BAD_REQUEST,
            "file.not_duplicate",
            "没有另一份在位的独立副本，拒绝删除",
        );
    }
    let path = row.path.to_string_lossy();
    if let Err(error) = file_delete::remove_file_and_ledger(&store, &path) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "file.delete", &error);
    }
    ok(json!({ "deleted": true, "path": row.path.display().to_string() })).into_response()
}
