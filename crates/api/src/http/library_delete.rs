//! Delete one Library item's files through the durable, version-checked queue.
//! Media, Playback, Collection and Subscribe facts survive the last file.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{LibraryId, MediaId};
use serde_json::json;
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;

fn item_files(
    store: &crate::Store,
    library_id: &str,
    media_id: MediaId,
) -> Result<Vec<domain::FileCoverage>, Response> {
    let missing = || {
        err(
            StatusCode::NOT_FOUND,
            "library.missing",
            "条目不属于该媒体库",
        )
    };
    let library = store
        .get_library(library_id)
        .map_err(store_error)?
        .ok_or_else(missing)?;
    if !store
        .library_accepts_work(library_id)
        .map_err(store_error)?
    {
        return Err(err(
            StatusCode::CONFLICT,
            "library.deleting",
            "Library 正在删除",
        ));
    }
    let media = store
        .get_media(media_id)
        .map_err(store_error)?
        .ok_or_else(missing)?;
    let library_id = library
        .id
        .parse::<LibraryId>()
        .map_err(|error| store_error(error.into()))?;
    let rows = store
        .file_coverage_in_library(
            library_id,
            Some(media_id),
            store::FileReadScope::AllRecorded,
        )
        .map_err(store_error)?;
    let files: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            super::library::library_for_file_strict(store, &row.file, &media)
                .is_some_and(|owner| owner.id == library.id)
        })
        .collect();
    if files.is_empty() {
        return Err(missing());
    }
    Ok(files)
}

fn store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "读取 Library 删除对象失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

pub(crate) async fn delete_item(
    State(state): State<ApiState>,
    Path((library_id, item_id)): Path<(String, String)>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let files = match super::store_wait::store_read(&state, move |store| {
        item_files(store, &library_id, media_id)
    })
    .await
    {
        Ok(Ok(files)) => files,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let report =
        match super::file_delete::delete_coverages(&state, files, "library_item_deleted").await {
            Ok(report) => report,
            Err(response) => return response,
        };
    tracing::info!(%media_id, removed=report.removed, pending_errors=report.errors.len(),
        "Library 条目文件删除执行结束");
    ok(json!({
        "removed_paths": report.paths,
        "rows_deleted": report.removed,
        "freed_bytes": report.freed_bytes,
        "errors": report.errors,
    }))
    .into_response()
}
