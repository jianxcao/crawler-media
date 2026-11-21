//! Library-wide stream information extraction, admitted in bounded file pages.
use axum::Json;
use axum::extract::{Path, State, rejection::JsonRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{FilePublication, LedgerId, LibraryId};
use serde::{Deserialize, Serialize};

use crate::http::{err, ok, store_wait};
use crate::management::ApiState;
use crate::probe_manager::policy::ProbeRequestResult;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RefreshMode {
    Missing,
    Force,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefreshRequest {
    mode: RefreshMode,
    request_id: String,
    after: Option<LedgerId>,
}

#[derive(Serialize)]
struct Receipt {
    mode: RefreshMode,
    total: usize,
    queued: usize,
    skipped: usize,
    next_cursor: Option<String>,
}

pub(crate) async fn refresh_library_media_info(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    request: Result<Json<RefreshRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(error) => return err(error.status(), "library.invalid", &error.body_text()),
    };
    if uuid::Uuid::parse_str(&request.request_id).is_err() {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "library.invalid",
            "request_id 必须是 UUID",
        );
    }
    let worker = state.clone();
    match state
        .fs_work
        .run_to_completion(move || refresh(&worker, &id, request))
        .await
    {
        Ok(Ok(receipt)) => ok(serde_json::json!(receipt)).into_response(),
        Ok(Err(response)) => response,
        Err(error) => super::browse_work_error(error),
    }
}

fn refresh(state: &ApiState, id: &str, request: RefreshRequest) -> Result<Receipt, Response> {
    state.require_writable()?;
    state.require_write_admitted()?;
    store_wait::with_store_read(state, &[store::Db::Library], |store| {
        let Some(_) = store.get_library(id).map_err(store_error)? else {
            return Err(super::missing_library());
        };
        if !store.library_accepts_work(id).map_err(store_error)? {
            return Err(err(
                StatusCode::CONFLICT,
                "library.deleting",
                "Library 正在删除",
            ));
        }
        Ok(())
    })??;
    let library_id = id
        .parse::<LibraryId>()
        .map_err(|_| super::missing_library())?;
    let mode = request.mode;
    let request_id = uuid::Uuid::parse_str(&request.request_id).map_err(|_| {
        err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "library.invalid",
            "request_id 必须是 UUID",
        )
    })?;
    let mut page = store_wait::with_store_read(state, &[store::Db::Library], |store| {
        store.list_file_coverage_page(library_id, request.after, 51)
    })?
    .map_err(store_error)?;
    let next_cursor = if page.len() > 50 {
        page.truncate(50);
        page.last().map(|row| row.file.file_id.to_string())
    } else {
        None
    };
    let mut receipt = Receipt {
        mode,
        total: 0,
        queued: 0,
        skipped: 0,
        next_cursor,
    };
    for row in page {
        if row.file.publication != FilePublication::Published {
            continue;
        }
        receipt.total += 1;
        let outcome = store_wait::with_foreground_store(state, |_| {
            state.probe.request_media_info_refresh(
                &row,
                matches!(mode, RefreshMode::Force),
                &request_id,
            )
        })?
        .map_err(|error| match error {
            crate::probe_manager::MediaInfoRefreshError::SourceChanged => err(
                StatusCode::CONFLICT,
                "library.probe_source_changed",
                "源文件已变化，请先扫描媒体库后再抓取媒体信息",
            ),
            crate::probe_manager::MediaInfoRefreshError::Store(error) => store_error(error),
        })?;
        if matches!(outcome, ProbeRequestResult::Queued { .. }) {
            receipt.queued += 1;
        } else {
            receipt.skipped += 1;
        }
    }
    tracing::info!(
        library_id = id,
        total = receipt.total,
        queued = receipt.queued,
        skipped = receipt.skipped,
        "媒体库媒体信息抓取已持久化入队"
    );
    Ok(receipt)
}

fn store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "媒体库媒体信息抓取入队失败");
    if matches!(
        error,
        store::StoreError::WriteBusy
            | store::StoreError::ReadBusy
            | store::StoreError::ReadClosed
            | store::StoreError::ReadDeadline
            | store::StoreError::ReadCancelled
    ) {
        return store_wait::busy_response();
    }
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "library.probe_enqueue_failed",
        "媒体信息抓取入队失败，请稍后重试",
    )
}
