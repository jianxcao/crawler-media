use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{FileCoverage, Media, MediaId};
use serde_json::{Value, json};

use crate::http::{err, ok_list, store_wait};
use crate::{ApiState, Store};

struct Captured {
    files: Vec<FileCoverage>,
    media: HashMap<MediaId, Media>,
}

fn capture(store: &Store, library: &str) -> Result<Captured, Response> {
    let library = store
        .get_library(library)
        .map_err(store_error)?
        .ok_or_else(super::super::library::missing_library)?;
    let id = library
        .id
        .parse()
        .map_err(|_| super::super::library::missing_library())?;
    let files = store
        .file_coverage_in_library(id, None, store::FileReadScope::AllRecorded)
        .map_err(store_error)?;
    let ids: HashSet<_> = files
        .iter()
        .map(|coverage| coverage.file.media_id)
        .collect();
    let media = store
        .media_batch(&ids.into_iter().collect::<Vec<_>>())
        .map_err(store_error)?;
    Ok(Captured { files, media })
}

fn store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "读取重复文件事实失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

/// Capture database facts off-executor, then observe each physical file once off-lock.
pub(crate) async fn duplicates(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let captured = match store_wait::store_read(&state, move |store| capture(store, &id)).await {
        Ok(Ok(captured)) => captured,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let files = state.file_stats.clone();
    match super::super::fs::read_work(&state, move || group(captured, files.as_ref())).await {
        Ok(groups) => ok_list(groups).into_response(),
        Err(error) => super::super::library::browse_work_error(error),
    }
}

fn group(captured: Captured, files: &dyn crate::file_stat_read::FileStatRead) -> Vec<Value> {
    let observed: Vec<_> = captured
        .files
        .iter()
        .map(|coverage| super::observe_file(files, &coverage.file.path))
        .collect();
    let mut units: HashMap<(MediaId, domain::UnitKey), Vec<usize>> = HashMap::new();
    for (index, coverage) in captured.files.iter().enumerate() {
        for unit in &coverage.units {
            units
                .entry((coverage.file.media_id, *unit))
                .or_default()
                .push(index);
        }
    }
    let mut groups = Vec::new();
    for ((media_id, unit), indexes) in units {
        let distinct: HashSet<_> = indexes
            .iter()
            .filter_map(|&index| observed[index].entity)
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        let Some(media) = captured.media.get(&media_id) else {
            continue;
        };
        let rows: Vec<_> = indexes
            .into_iter()
            .map(|index| {
                let row = &captured.files[index].file;
                json!({
                    "file_id": row.file_id.to_string(), "path":row.path.display().to_string(),
                    "resolution":row.resolution, "codec":row.codec, "hdr":row.hdr,
                    "quality_source":row.quality_source.as_str(), "size_bytes":observed[index].size,
                })
            })
            .collect();
        groups.push(json!({
            "media_item_id":media.id.to_string(), "title":media.title,
            "season":(!unit.is_whole()).then_some(unit.season),
            "episode":(!unit.is_whole()).then_some(unit.episode), "files":rows,
        }));
    }
    groups
}
