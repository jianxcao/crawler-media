//! Paginated `/api/v1/ledger/transfers` endpoint with SQL filtering, work grouping,
//! and STRM-aware zero-local-byte accounting.

use std::str::FromStr;
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::file_stat_read::FileStatRead;
use crate::http::{err, fs, library, ok, store_wait};
use crate::management::ApiState;

const DEFAULT_LIMIT: usize = 50;
const MIN_LIMIT: usize = 1;
const MAX_LIMIT: usize = 200;

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ListTransfersQuery {
    view: Option<String>,
    q: Option<String>,
    mode: Option<String>,
    kind: Option<String>,
    media_id: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

/// GET /ledger/transfers — paginated, filtered, STRM-aware transfer view.
pub(crate) async fn list_transfers(
    State(state): State<ApiState>,
    Query(raw): Query<ListTransfersQuery>,
) -> Response {
    let page_query = match parse_query(raw) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };

    let sql_started = Instant::now();
    let page = match store_wait::store_read(&state, &[store::Db::Library], move |store| {
        store.transfer_page(&page_query)
    })
    .await
    {
        Ok(Ok(page)) => page,
        Ok(Err(store_error)) => return super::admin_store_error(store_error),
        Err(call_error) => return call_error.response(),
    };
    let sql_ms = sql_started.elapsed().as_secs_f64() * 1000.0;

    let needs_fs_stat =
        page.view == store::TransferViewMode::Files && page.files.iter().any(|row| !row.is_strm);
    if !needs_fs_stat {
        tracing::debug!(
            view = page.view.as_str(),
            sql_ms,
            returned_files = page.files.len(),
            returned_groups = page.groups.len(),
            "transfers page served from SQL snapshot without fs stat"
        );
        return build_response(page, None);
    }

    let files = state.file_stats.clone();
    match fs::read_work(&state, move || {
        let observe_started = Instant::now();
        let response = build_response(page, Some(files.as_ref()));
        let observe_ms = observe_started.elapsed().as_secs_f64() * 1000.0;
        tracing::debug!(sql_ms, observe_ms, "transfers page response completed");
        response
    })
    .await
    {
        Ok(response) => response,
        Err(error) => library::browse_work_error(error),
    }
}

fn parse_query(raw: ListTransfersQuery) -> Result<store::TransferPageQuery, Response> {
    let view = match raw.view.as_deref().map(str::trim) {
        None | Some("") | Some("files") => store::TransferViewMode::Files,
        Some("groups") => store::TransferViewMode::Groups,
        Some(_) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "ledger.invalid_query",
                "view 参数无效，仅支持 files 或 groups",
            ));
        }
    };

    let mode = match raw.mode.as_deref().map(str::trim) {
        None | Some("") | Some("all") => store::TransferModeFilter::All,
        Some("hardlink") => store::TransferModeFilter::Hardlink,
        Some("copy") => store::TransferModeFilter::Copy,
        Some("move") => store::TransferModeFilter::Move,
        Some("strm") => store::TransferModeFilter::Strm,
        Some(_) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "ledger.invalid_query",
                "mode 参数无效",
            ));
        }
    };

    let kind = match raw.kind.as_deref().map(str::trim) {
        None | Some("") | Some("all") => store::TransferKindFilter::All,
        Some("movie") => store::TransferKindFilter::Movie,
        Some("tv") => store::TransferKindFilter::Tv,
        Some(_) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "ledger.invalid_query",
                "kind 参数无效",
            ));
        }
    };

    let media_id = match raw
        .media_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(value) => Some(domain::MediaId::from_str(value).map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "ledger.invalid_query",
                "media_id 参数无效",
            )
        })?),
    };

    let offset = raw.offset.unwrap_or(0);
    let limit = raw
        .limit
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(MIN_LIMIT, MAX_LIMIT);
    let search = raw
        .q
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty());

    Ok(store::TransferPageQuery {
        view,
        search,
        mode,
        kind,
        media_id,
        offset,
        limit,
    })
}

fn build_response(page: store::TransferPage, files: Option<&dyn FileStatRead>) -> Response {
    let items: Vec<Value> = page
        .files
        .into_iter()
        .map(|row| serialize_file_row(row, &page.global_transfer_mode, files))
        .collect();
    let groups: Vec<Value> = page.groups.into_iter().map(serialize_group_row).collect();

    ok(json!({
        "summary": page.summary,
        "view": page.view.as_str(),
        "filtered_total": page.filtered_total,
        "filtered_file_total": page.filtered_file_total,
        "filtered_group_total": page.filtered_group_total,
        "offset": page.offset,
        "limit": page.limit,
        "has_more": page.has_more,
        "items": items,
        "groups": groups,
    }))
    .into_response()
}

fn serialize_file_row(
    row: store::TransferFileRow,
    global_transfer_mode: &str,
    files: Option<&dyn FileStatRead>,
) -> Value {
    let coverage = row.coverage;
    let file = coverage.file;
    let path_json = file.path.to_string_lossy().into_owned();
    let source_path = file
        .source_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());

    let (transfer_mode, file_size) = if row.is_strm {
        ("strm".to_string(), Value::Null)
    } else {
        let meta = files.and_then(|fs| fs.metadata(&file.path).ok());
        let size = file
            .size_bytes
            .or_else(|| meta.as_ref().map(|m| m.len()))
            .unwrap_or(0);
        let mode = match files {
            Some(fs) => super::transfer_mode_label(
                false,
                meta.as_ref(),
                file.source_path.as_deref(),
                file.transfer_mode,
                global_transfer_mode,
                fs,
            ),
            None => file
                .transfer_mode
                .unwrap_or_else(|| global_transfer_mode.to_string()),
        };
        (mode, json!(size))
    };

    json!({
        "id": file.file_id.to_string(),
        "media_id": file.media_id.to_string(),
        "library_id": file.library_id.to_string(),
        "media_title": row.media_title,
        "media_kind": row.media_kind.as_str(),
        "path": path_json,
        "source_path": source_path,
        "units": coverage.units,
        "resolution": file.resolution,
        "codec": file.codec,
        "hdr": file.hdr,
        "quality_source": file.quality_source.as_str(),
        "confidence": file.confidence.as_str(),
        "filter_score": file.filter_score,
        "is_strm": row.is_strm,
        "transfer_mode": transfer_mode,
        "file_size": file_size,
        "duration_ms": row.effective_duration_ms,
    })
}

fn serialize_group_row(group: store::TransferGroupRow) -> Value {
    json!({
        "media_id": group.media_id.to_string(),
        "media_title": group.media_title,
        "media_kind": group.media_kind.as_str(),
        "library_id": group.library_id.to_string(),
        "file_count": group.file_count,
        "strm_count": group.strm_count,
        "hardlink_count": group.hardlink_count,
        "copy_count": group.copy_count,
        "move_count": group.move_count,
        "local_size_bytes": group.local_size_bytes,
        "total_duration_ms": group.total_duration_ms,
        "primary_resolution": group.primary_resolution,
        "primary_mode": group.primary_mode,
        "sample_dir": group.sample_dir,
    })
}
