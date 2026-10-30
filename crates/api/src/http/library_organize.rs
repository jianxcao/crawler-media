//! Organize: rename existing library files per the effective naming template
//! (preview then apply), plus single-item metadata refresh and the per-library
//! image gallery.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};
use std::str::FromStr;

use crate::blocking_budget::BlockingError;
use crate::http::library::{
    coverages_in_library, library_for_file, preferred_coverage, require_visible_library,
};
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

use super::library::missing_library;

/// 只读整理预览/放宽建议/相似推荐的等待预算。
const ORGANIZE_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Blocking-budget failure → HTTP response（上游/存储错误走各自原有路径）。
fn blocking_error(error: BlockingError, code: &'static str) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(StatusCode::GATEWAY_TIMEOUT, code, "整理请求超时"),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

fn release_for(coverage: &domain::FileCoverage) -> domain::Release {
    let name = coverage
        .file
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let mut parsed = release::parse(name);
    parsed.resolution = coverage.file.resolution.clone().or(parsed.resolution);
    parsed.codec = coverage.file.codec.clone().or(parsed.codec);
    parsed.hdr = coverage.file.hdr.clone().or(parsed.hdr);
    parsed
}

/// GET /libraries/{id}/organize-preview — target names for every owned file
/// under the library's naming template.
pub(crate) async fn preview_organize(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run(ORGANIZE_READ_DEADLINE, move || {
            preview_organize_inner(&state_for_work, &id)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.timeout"),
    }
}

fn preview_organize_inner(state: &ApiState, id: &str) -> Response {
    let store = state.store.as_ref();
    let Some(library) = store.get_library(id).ok().flatten() else {
        return missing_library();
    };
    let pattern = match store.naming_pattern(library.kind) {
        Ok(pattern) => pattern,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let primary = library.root_paths.first().cloned().unwrap_or_default();
    let mut renames = Vec::new();
    let mut already_ok = 0usize;
    let mut skips = 0usize;
    for coverage in coverages_in_library(&store, &library) {
        let row = &coverage.file;
        let Some(media) = store.get_media(row.media_id).ok().flatten() else {
            continue;
        };
        let release = release_for(&coverage);
        let target = match library::render_path(
            &primary,
            &pattern,
            &media,
            &release,
            FsPath::new(&row.path),
        ) {
            Ok(target) => target,
            Err(_) => {
                skips += 1;
                continue;
            }
        };
        if target.to_string_lossy() == row.path.to_string_lossy() {
            already_ok += 1;
            continue;
        }
        renames.push(json!({
            "from": row.path,
            "to": target.display().to_string(),
            "title": media.title,
            "kind": media.kind.as_str(),
        }));
    }
    ok(json!({
        "total": renames.len() + already_ok + skips,
        "already_ok": already_ok,
        "skips": skips,
        "renames": renames,
    }))
    .into_response()
}

/// POST /libraries/{id}/organize — apply the previewed renames. Body:
/// `{"renames": [{"from","to","to_library_id"?}, ...]}`. `from` must belong to
/// `{id}`; `to` must belong to `{id}` unless the entry names another Library of the
/// same kind in `to_library_id` (admin-only route; durable cross-Library re-parent).
pub(crate) async fn organize(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run_to_completion(move || {
            super::library_organize_apply::organize_inner(&state_for_work, &id, &body)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.worker"),
    }
}

/// POST /libraries/{id}/items/{item_id}/metadata/refresh — single item:
/// poster + backdrop + rich NFO from TMDB.
pub(crate) async fn refresh_item_metadata(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run_to_completion(move || refresh_item_metadata_inner(&state_for_work, &id, &item_id))
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.worker"),
    }
}

/// 整个同步刷新用例（lease → TMDB 抓取 → 海报/NFO 落盘）在 blocking 闭包内，
/// LibraryLease 持有到真实工作结束。
fn refresh_item_metadata_inner(state: &ApiState, id: &str, item_id: &str) -> Response {
    let lease = match state.acquire_library_work(id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.frozen", &error),
    };
    let store = state.store.as_ref();
    let Some(library) = store.get_library(id).ok().flatten() else {
        return missing_library();
    };
    let media_id = match domain::MediaId::from_str(item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let rows: Vec<domain::FileCoverage> = coverages_in_library(&store, &library)
        .into_iter()
        .filter(|row| row.file.media_id == media_id)
        .collect();
    let Some(row) = preferred_coverage(&rows).cloned() else {
        return err(
            StatusCode::NOT_FOUND,
            "library.item_missing",
            "条目没有在位文件",
        );
    };
    let path = row.file.path.clone();
    // 剧集级目录：模板整理后是 `<show>/Season N/file`，show 根 = 文件父目录的上一级；
    // 平铺时 = 文件所在目录。poster/fanart/tvshow.nfo 落在 show 根。
    let file_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let show_root: PathBuf = if media.kind == domain::MediaKind::Tv {
        crate::scrape_metadata::show_root(&path, &row)
    } else {
        file_dir.clone()
    };
    let mut refreshed = 0;
    if let Some(bytes) = crate::poster_fetch::attach_poster(&state, &media, &path) {
        refreshed += 1;
        let _ = lease.control().signal().publish(|| {
            std::fs::write(show_root.join("poster.jpg"), &bytes).map_err(|error| error.to_string())
        });
    }
    if let Some(bytes) = crate::poster_fetch::force_attach_backdrop(&state, &media, &path) {
        let _ = lease.control().signal().publish(|| {
            std::fs::write(show_root.join("fanart.jpg"), &bytes).map_err(|error| error.to_string())
        });
    }
    if let Some(tmdb_id) = media.tmdb_id.as_deref() {
        match crate::scrape_metadata::fetch_tmdb_metadata(&state, media.kind, tmdb_id) {
            Ok(Some(meta)) => {
                let meta_service = crate::metadata_service::MetadataService::new(
                    std::sync::Arc::new(state.clone()),
                );
                let _ = meta_service.refresh_media_metadata(&media);

                let nfo = crate::scrape_metadata::nfo_from_tmdb(&media, &meta);
                if media.kind == domain::MediaKind::Tv {
                    crate::scrape_metadata::scrape_tv_sidecars(&state, &media, &show_root, &rows);
                } else if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                    let target = path.with_file_name(format!("{stem}.nfo"));
                    // 同 library_scan：先捕获用户在 NFO 里的改动，write_nfo 之后再把生效
                    // 元数据整体导回，用户改的那一项才不会在刷新时被 provider 覆盖掉。
                    let export = crate::scrape_metadata::export::begin_sidecar_export(
                        &state,
                        &media,
                        &path,
                        domain::EntityScope::Work,
                    );
                    // Ledgered videos go through the guarded export driver only; no
                    // second, unguarded write of the same sidecar.
                    match export {
                        Ok(crate::scrape_metadata::export::ExportPreparation::Ledgered(
                            request,
                        )) => {
                            crate::scrape_metadata::export::finish_sidecar_export(&state, request)
                        }
                        // Preparation failed: already logged; this file's sidecar stays as is.
                        Err(_) => {}
                        Ok(crate::scrape_metadata::export::ExportPreparation::NotLedgered) => {
                            let _ = lease.control().signal().publish(|| {
                                crate::scrape_metadata::write_nfo(&target, &media, &nfo);
                                Ok(())
                            });
                        }
                    }
                    let movie_root = path.parent().unwrap_or(&path);
                    crate::scrape_metadata::scrape_movie_fanart(&state, &media, movie_root);
                }
            }
            Ok(None) => {
                tracing::debug!(%tmdb_id, "catalog returned no metadata for library item refresh")
            }
            Err(error) => {
                tracing::warn!(%error, %tmdb_id, "failed to refresh library item metadata")
            }
        }
    }
    ok(json!({ "refreshed": refreshed })).into_response()
}

/// POST /libraries/{id}/items/relax-filter — 筛空时的放宽建议：对每个已激活的
/// 服务端可算取值（分辨率档 / HDR），算「只去掉它还能留下多少部」。响应形状
/// 对齐前端 FilterEmptyState：`{total, suggestions: [{dim, dim_label, value, label, count}]}`。
pub(crate) async fn relax_filter(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run(ORGANIZE_READ_DEADLINE, move || {
            relax_filter_inner(&state_for_work, &id, &body)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.timeout"),
    }
}

fn relax_filter_inner(state: &ApiState, id: &str, body: &Value) -> Response {
    let store = state.store.as_ref();
    let Some(library) = store.get_library(id).ok().flatten() else {
        return missing_library();
    };
    let resolutions: Vec<String> = body["resolutions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let want_hdr: Option<bool> = body["hdr"].as_bool();
    let mut by_media: std::collections::HashMap<domain::MediaId, Vec<domain::FileCoverage>> =
        std::collections::HashMap::new();
    for coverage in coverages_in_library(&store, &library) {
        by_media
            .entry(coverage.file.media_id)
            .or_default()
            .push(coverage);
    }
    let count_with = |drop_resolutions: &[String], drop_hdr: bool| -> usize {
        by_media
            .values()
            .filter(|rows| {
                let keeps_res = drop_resolutions.is_empty()
                    || rows.iter().any(|row| {
                        row.file
                            .resolution
                            .as_deref()
                            .is_some_and(|r| drop_resolutions.iter().any(|w| w == r))
                    });
                if !keeps_res {
                    return false;
                }
                if !drop_hdr {
                    return true;
                }
                let has_hdr = rows.iter().any(|row| row.file.hdr.is_some());
                has_hdr == want_hdr.unwrap_or(false) || want_hdr.is_none()
            })
            .count()
    };
    let active = !resolutions.is_empty() || want_hdr.is_some();
    let mut suggestions = Vec::new();
    if active {
        for res in &resolutions {
            let kept: Vec<String> = resolutions
                .iter()
                .filter(|r| r.as_str() != res.as_str())
                .cloned()
                .collect();
            suggestions.push(json!({
                "dim": "resolutions",
                "dim_label": "分辨率",
                "value": res,
                "label": res,
                "count": count_with(&kept, true),
            }));
        }
        if let Some(hdr) = want_hdr {
            suggestions.push(json!({
                "dim": "hdr",
                "dim_label": "HDR",
                "value": hdr.to_string(),
                "label": if hdr { "只看 HDR" } else { "只看 SDR" },
                "count": count_with(&resolutions, false),
            }));
        }
    }
    ok(json!({
        "total": by_media.len(),
        "suggestions": suggestions,
    }))
    .into_response()
}

/// GET /libraries/{id}/items/{itemId}/similar — TMDB similar titles as
/// poster-wall hits（库条目详情页的「相似推荐」行）。
pub(crate) async fn similar(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run(ORGANIZE_READ_DEADLINE, move || {
            similar_inner(&state_for_work, &id, &item_id, user_id)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.timeout"),
    }
}

fn similar_inner(
    state: &ApiState,
    id: &str,
    item_id: &str,
    user_id: Option<domain::UserId>,
) -> Response {
    let store = state.store.as_ref();
    let library = match require_visible_library(&store, id, user_id) {
        Ok(library) => library,
        Err(response) => return response,
    };
    let media_id = match domain::MediaId::from_str(item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let rows = match store.file_coverage_for_media(media_id) {
        Ok(rows) => rows,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let belongs_to_library = rows.iter().any(|row| {
        library_for_file(&store, &row.file, &media).is_some_and(|owner| owner.id == library.id)
    });
    if !belongs_to_library {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    }
    let Some(tmdb_id) = media.tmdb_id.clone() else {
        return ok_list(Vec::new()).into_response();
    };
    let hits = state
        .catalog
        .similar(media.kind, &tmdb_id)
        .unwrap_or_default();
    let items: Vec<Value> = hits
        .into_iter()
        .map(|hit| {
            json!({
                "id": hit.media.tmdb_id,
                "media_item_id": null,
                "kind": hit.media.kind.as_str(),
                "title": hit.media.title,
                "year": hit.media.year,
                "tmdb_id": hit.media.tmdb_id,
                "poster_url": hit.poster_path.as_deref().map(|p| format!("https://image.tmdb.org/t/p/w342{p}")),
            })
        })
        .collect();
    ok_list(items).into_response()
}
