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

fn file_entity(path: &str) -> Option<std::path::PathBuf> {
    let path = FsPath::new(path);
    if !path.exists() {
        return None;
    }
    std::fs::canonicalize(path).ok()
}

fn distinct_present_entities(rows: &[domain::LedgerRow]) -> Vec<std::path::PathBuf> {
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

/// GET /libraries/{id}/duplicates — ledger rows sharing the same
/// (media, season, episode) unit, grouped for keep/delete decisions.
pub(crate) async fn duplicates(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let store = state.store.lock();
    let Some(library) = store.get_library(&id).ok().flatten() else {
        return missing_library();
    };
    let mut units: HashMap<(domain::MediaId, Option<u32>, Option<u32>), Vec<domain::LedgerRow>> =
        HashMap::new();
    for row in super::library::rows_in_library(&store, &library) {
        units
            .entry((row.media_id, row.season, row.episode))
            .or_default()
            .push(row);
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
                    "file_id": row.id.to_string(),
                    "path": row.path,
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
    let Some(row) = store.get_ledger(&file_id.replace('-', "")).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "file.missing", "文件不存在");
    };
    let Some(media) = store.get_media(row.media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "file.missing", "文件不存在");
    };
    if crate::http::library::library_for_row_strict(&store, &row, &media)
        .is_none_or(|owner| owner.id != library.id)
    {
        return err(StatusCode::NOT_FOUND, "file.missing", "文件不属于该媒体库");
    }
    let siblings: Vec<_> = super::library::rows_in_library(&store, &library)
        .into_iter()
        .filter(|other| {
            other.media_id == row.media_id
                && other.season == row.season
                && other.episode == row.episode
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
    if let Err(error) = file_delete::remove_file_and_ledger(&store, &row.path) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "file.delete", &error);
    }
    ok(json!({ "deleted": true, "path": row.path })).into_response()
}
