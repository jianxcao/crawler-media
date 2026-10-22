//! Library maintenance: in-place scan, metadata/cover refresh, missing
//! episodes, index, and facet dimensions — all scoped to one Library.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

pub(crate) mod operations;
pub(crate) mod report;
pub(crate) mod runner;
pub(crate) mod scope;
pub(crate) mod video;
use super::library::{
    browse_filter_from_query, browse_order_from_query, browse_store_error, coverages_in_library,
    missing_library, preferred_coverage, require_visible_library,
};
use crate::store::library_query;
#[cfg(test)]
pub(crate) use runner::checkpoint_fresh;
pub(crate) use runner::scan_library_leased_resumable;
use scope::{ScanScope, walk_video_files};

/// POST /libraries/{id}/scan — walk every root of the library and ledger
/// high-confidence video files in place.
pub(crate) async fn scan_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    match state.scan_coordinator.enqueue(&state, &id, vec![]) {
        Ok(op) => (
            StatusCode::ACCEPTED,
            axum::Json(serde_json::json!({
                "ok": true,
                "data": {
                    "operation_id": op.id,
                    "status": op.status.as_str(),
                    "queued": true,
                    "transferred": false,
                }
            })),
        )
            .into_response(),
        Err(crate::scan_operations::ScanEnqueueError::NotFound(e)) => {
            err(StatusCode::NOT_FOUND, "library.missing", &e)
        }
        Err(crate::scan_operations::ScanEnqueueError::Conflict(e)) => {
            err(StatusCode::CONFLICT, "library.deleting", &e)
        }
        Err(crate::scan_operations::ScanEnqueueError::Internal(e)) => {
            err(StatusCode::INTERNAL_SERVER_ERROR, "library.scan", &e)
        }
    }
}

/// 同步扫描一个库根（子目录扫描的整根特例）。
///
/// 生产入口是 `runner::scan_library_leased_resumable`（经 ScanCoordinator）；
/// 这里保留给 watch 子目录扫描的单元测试与手动维护调用。
#[allow(dead_code)]
pub(crate) fn scan_library_root(state: &ApiState, root: &std::path::Path) -> Result<(), String> {
    scan_library_subdir(state, root, root)
}

/// 路径 → 覆盖记录（含该文件已入库的 unit）。旧 `ledger_row_by_path` 的单值
/// `(season, episode)` 读数在这里被 `FileCoverage.units` 取代。
fn coverage_by_path(store: &crate::Store, path: &str) -> Option<domain::FileCoverage> {
    store.file_coverage_by_path(path).unwrap_or_else(|error| {
        tracing::warn!(%error, path, "扫描期间读取台账行失败");
        None
    })
}

fn media_by_id(store: &crate::Store, media_id: domain::MediaId) -> Option<domain::Media> {
    store.get_media(media_id).unwrap_or_else(|error| {
        tracing::warn!(%error, media_id = %media_id, "扫描期间读取 Media 失败");
        None
    })
}

fn library_by_id(store: &crate::Store, id: &str) -> Option<store::Library> {
    store.get_library(id).unwrap_or_else(|error| {
        tracing::warn!(%error, library_id = id, "扫描期间读取 Library 失败");
        None
    })
}

pub(crate) fn scan_library_subdir(
    state: &ApiState,
    root: &std::path::Path,
    target_dir: &std::path::Path,
) -> Result<(), String> {
    let (library_id, kind, scrape) = {
        let store = state.store.lock();
        let library = store
            .list_libraries()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|library| {
                library.realtime_watch
                    && library.root_paths.iter().any(|candidate| candidate == root)
            })
            .ok_or_else(|| format!("no realtime Library configured for root {}", root.display()))?;
        let scrape = store
            .get_scrape_config()
            .map_err(|error| error.to_string())?
            .effective
            .mirror_nfo;
        (library.id, library.kind, scrape)
    };
    let lease = state.acquire_library_work(&library_id)?;
    if !target_dir.is_dir() {
        return Ok(());
    }
    if kind == domain::MediaKind::Video {
        return scan_video_subdir(state, target_dir, root, &library_id, &lease);
    }
    let probe = domain::Media {
        id: domain::MediaId::new(),
        kind,
        title: "watch scan".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let job = library::WatchJob {
        kind: library::WatchKind::InPlace,
        library_root: root.to_path_buf(),
        tv_library_root: None,
        path: target_dir.to_path_buf(),
        scrape,
    };
    let outcome = library::scan_watch(&job, &probe).map_err(|error| error.to_string())?;
    let scan_scope = ScanScope::capture(state, &library_id).map_err(|e| format!("{:?}", e))?;
    let filtered_transferred = scan_scope
        .filter_transferred(outcome.transferred)
        .map_err(|e| format!("{:?}", e))?;

    let inserted = lease.publish(|| {
        let store = state.store.lock();
        scan_scope
            .validate(&store)
            .map_err(|e| format!("{:?}", e))?;
        crate::watch_ledger::record_paths(&store, filtered_transferred.clone())
    })?;

    let scan_media = {
        let store = state.store.lock();
        let mut map = HashMap::new();
        for file in &filtered_transferred {
            let Some(coverage) = coverage_by_path(&store, &file.path.display().to_string()) else {
                continue;
            };
            let Some(media) = media_by_id(&store, coverage.file.media_id) else {
                continue;
            };
            if media.kind != domain::MediaKind::Video {
                map.entry(media.id)
                    .or_insert((media, coverage.file.path.clone()));
            }
        }
        map
    };
    resolve_and_scrape_subdir(state, scan_media, &inserted, scrape, &lease)?;
    tracing::info!(
        root = %root.display(),
        target_dir = %target_dir.display(),
        scanned = filtered_transferred.len(),
        inserted = inserted.len(),
        "实时目录扫描完成，仅新增台账进入探测队列"
    );
    crate::http::library::enqueue_probes_for_paths(state, inserted);
    {
        let store = state.store.lock();
        if let Some(library) = library_by_id(&store, &library_id) {
            crate::http::library::clear_library_cover_checked(&store, &library.id);
            let _ = crate::http::library::library_cover_path(&store, &library);
        }
    }
    Ok(())
}

fn scan_video_subdir(
    state: &ApiState,
    target_dir: &std::path::Path,
    root: &std::path::Path,
    library_id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<(), String> {
    let mut files = Vec::new();
    walk_video_files(target_dir, &mut files);
    let scan_scope = ScanScope::capture(state, library_id).map_err(|e| format!("{:?}", e))?;
    let filtered_files = scan_scope
        .filter_paths(files)
        .map_err(|e| format!("{:?}", e))?;
    let inserted = lease.publish(|| {
        let store = state.store.lock();
        scan_scope
            .validate(&store)
            .map_err(|e| format!("{:?}", e))?;
        crate::watch_ledger::record_video_paths(&store, filtered_files)
    })?;
    tracing::info!(root = %root.display(), target_dir = %target_dir.display(),
        inserted = inserted.len(), "实时 Video Library 扫描完成，不进行 Movie/TV 识别");
    crate::http::library::enqueue_probes_for_paths(state, inserted);
    let store = state.store.lock();
    if let Some(library) = store
        .get_library(library_id)
        .map_err(|error| error.to_string())?
    {
        crate::http::library::clear_library_cover_checked(&store, &library.id);
        let _ = crate::http::library::library_cover_path(&store, &library);
    }
    Ok(())
}

fn resolve_and_scrape_subdir(
    state: &ApiState,
    scan_media: HashMap<domain::MediaId, (domain::Media, PathBuf)>,
    inserted: &[PathBuf],
    scrape: bool,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<(), String> {
    for (_, (media, path)) in scan_media {
        lease
            .control()
            .check()
            .map_err(|e: library::LibraryError| e.to_string())?;
        if crate::auto_resolve::has_local_scan_metadata(state, &media, &path) {
            continue;
        }
        let resolved = if media.tmdb_id.is_some() || media.douban_id.is_some() {
            Some(media.clone())
        } else {
            match crate::auto_resolve::resolve_scanned_media(state, &media, &path) {
                Some(resolved) => {
                    tracing::info!(title = %resolved.title, path = %path.display(), "实时扫描自动匹配媒体元数据成功");
                    Some(resolved)
                }
                None => {
                    tracing::warn!(title = %media.title, path = %path.display(), "实时扫描未能自动匹配媒体元数据");
                    None
                }
            }
        };

        if let Some(resolved_media) = resolved.as_ref()
            && let Err(error) =
                crate::auto_resolve::rebind_scan_rows(state, media.id, resolved_media.id, inserted)
        {
            return Err(error);
        }
        if scrape && let Some(resolved_media) = resolved {
            // 真实失败（取路径租约失败、落盘失败）由 poster_fetch 内部按 warn 记录；
            // None 也包含"本来就没有可写的图"，不是错误。
            let _ = crate::poster_fetch::attach_poster(state, &resolved_media, &path);
            let _ = crate::poster_fetch::attach_backdrop(state, &resolved_media, &path);
            let (target_coverage, all_coverages) = {
                let store = state.store.lock();
                let coverages = match store.file_coverage_for_media(resolved_media.id) {
                    Ok(coverages) => coverages,
                    Err(error) => {
                        tracing::warn!(%error, media_id = %resolved_media.id, "读取 Media 台账行失败");
                        Vec::new()
                    }
                };
                let target = coverages
                    .iter()
                    .find(|coverage| coverage.file.path.as_path() == path)
                    .cloned();
                (target, coverages)
            };
            if let Some(coverage) = target_coverage {
                scrape_library_metadata(state, &resolved_media, &coverage, &path, &all_coverages);
            }
        }
    }
    Ok(())
}

/// POST /libraries/{id}/metadata/refresh — one cover per Media owned under the
/// library, using the newest owned episode for TV; also writes rich NFO from
/// TMDB details and episode stills for owned TV episodes.
pub(crate) async fn refresh_metadata(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let lease = match state.acquire_library_work(&id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.deleting", &error),
    };
    let store = state.store.lock();
    let Some(library) = library_by_id(&store, &id) else {
        return missing_library();
    };
    // 读数口径保持 `list_file_coverage()` + library 归属过滤：这就是 Task 18/19/20
    // 基线记录的 `root_ledger_scans` 路径，换成 `file_coverage_for_media` 会让已冻结的
    // store 侧读数与这里的 api 侧读数不可比，并且丢掉 library 归属过滤。
    let owned_coverages = coverages_in_library(&store, &library);
    let mut grouped: HashMap<domain::MediaId, Vec<domain::FileCoverage>> = HashMap::new();
    for coverage in &owned_coverages {
        grouped
            .entry(coverage.file.media_id)
            .or_default()
            .push(coverage.clone());
    }
    let mut work = Vec::new();
    for (media_id, media_coverages) in grouped {
        let Some(media) = media_by_id(&store, media_id) else {
            continue;
        };
        let live_coverages: Vec<_> = media_coverages
            .iter()
            .filter(|coverage| crate::scrape_metadata::targets::video_present(&coverage.file.path))
            .collect();
        if let Some(coverage) = preferred_coverage(&live_coverages) {
            work.push((media, (*coverage).clone()));
        }
    }
    drop(store);
    let mut refreshed = 0u32;
    for (media, coverage) in work {
        if lease.control().is_cancelled()
            || !state
                .store
                .lock()
                .library_accepts_work(&id)
                .unwrap_or(false)
        {
            return err(
                StatusCode::CONFLICT,
                "library.deleting",
                "Library 正在删除或已冻结",
            );
        }
        let path = coverage.file.path.clone();
        if crate::poster_fetch::attach_poster(&state, &media, &path).is_some() {
            refreshed += 1;
        }
        // 真实失败由 poster_fetch 内部记录；None 表示没有可写的图。
        let _ = crate::poster_fetch::force_attach_backdrop(&state, &media, &path);
        // TMDB 详情 + 演职员写入 NFO（无 NFO 时也补一份），供条目详情页读取。
        scrape_library_metadata(&state, &media, &coverage, &path, &owned_coverages);
        if lease.control().is_cancelled() {
            return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
        }
        // 章节场景图自动生成（有内嵌章节且无帧时）。
        crate::http::library_chapters::auto_generate_chapters(&state, &path);
    }
    ok(json!({ "queued": true, "refreshed": refreshed })).into_response()
}

fn scrape_library_metadata(
    state: &ApiState,
    media: &domain::Media,
    coverage: &domain::FileCoverage,
    path: &std::path::Path,
    owned_coverages: &[domain::FileCoverage],
) {
    let lease = match state.acquire_path_work(path, media.kind) {
        Ok(lease) => lease,
        Err(_) => return,
    };
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    if media.kind == domain::MediaKind::Tv {
        // S4 目标签名（`scrape_metadata::show_root(path, coverage)` / `scrape_tv_sidecars(state, media,
        // show_root, coverages)`）在 task-79 落地前这两行按目标形状传参，报错归属 S4。
        let show_root = crate::scrape_metadata::show_root(path, coverage);
        let episodes = owned_coverages
            .iter()
            .filter(|owned| owned.file.media_id == media.id)
            .cloned()
            .collect::<Vec<_>>();
        crate::scrape_metadata::scrape_tv_sidecars(state, media, &show_root, &episodes);
        return;
    }
    match crate::scrape_metadata::fetch_tmdb_metadata(state, media.kind, tmdb_id) {
        Ok(Some(metadata)) => {
            if lease
                .as_ref()
                .is_some_and(|lease| lease.control().is_cancelled())
            {
                return;
            }
            let nfo = crate::scrape_metadata::nfo_from_tmdb(media, &metadata);
            crate::scrape_metadata::adopt_tmdb_metadata(
                state,
                media,
                tmdb_id,
                domain::EntityScope::Work,
            );
            crate::scrape_metadata::record_scraped_display_meta(state, media.id, &metadata, &nfo);
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                let target = path.with_file_name(format!("{stem}.nfo"));
                // 先捕获用户对上一份 NFO 的改动，再整体重写并导出：write_nfo 会原地
                // 覆盖标量，捕获必须发生在它之前，否则改动无法进入 overrides。
                let export = crate::scrape_metadata::export::begin_sidecar_export(
                    state,
                    media,
                    path,
                    domain::EntityScope::Work,
                );
                // A ledgered video is exported only through the driver (CAS + digest
                // guard); the legacy in-place write is kept for unledgered files only.
                match export {
                    Some(request) => {
                        crate::scrape_metadata::export::finish_sidecar_export(state, request)
                    }
                    None => {
                        let write = || {
                            crate::scrape_metadata::write_nfo(&target, media, &nfo);
                            Ok(())
                        };
                        let _ = match &lease {
                            Some(lease) => lease.publish(write),
                            None => write(),
                        };
                    }
                }
                let movie_root = path.parent().unwrap_or(path);
                crate::scrape_metadata::scrape_movie_fanart(state, media, movie_root);
            }
        }
        Ok(None) => {
            tracing::debug!(%tmdb_id, "catalog returned no metadata during library refresh")
        }
        Err(error) => tracing::warn!(%error, %tmdb_id, "failed to scrape library metadata"),
    }
}

/// GET /libraries/{id}/missing — missing files in this library on disk.
pub(crate) async fn missing(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    let library = match require_visible_library(&store, &id, user_id) {
        Ok(library) => library,
        Err(response) => return response,
    };
    let missing_map = store.missing_at_by_path().unwrap_or_default();
    let mut media_groups: HashMap<domain::MediaId, (domain::Media, Vec<Value>)> = HashMap::new();
    for coverage in coverages_in_library(&store, &library) {
        if !missing_map.contains_key(coverage.file.path.to_string_lossy().as_ref()) {
            continue;
        }
        let Some(media) = store.get_media(coverage.file.media_id).ok().flatten() else {
            continue;
        };
        let entry = media_groups
            .entry(coverage.file.media_id)
            .or_insert_with(|| (media, Vec::new()));
        // 旧读模型里每个 (file × unit) 一条 JSON：整片（旧 season/episode 皆 None）
        // 记 (0, 0)，其余记 unit 的季/集，逐条展开保持条目集合不变。
        for unit in &coverage.units {
            entry.1.push(json!({
                "id": coverage.file.file_id.to_string(),
                "file_path": coverage.file.path,
                "season_number": if unit.is_whole() { 0 } else { unit.season },
                "episode_number": if unit.is_whole() { 0 } else { unit.episode },
                "size_bytes": 0,
            }));
        }
    }
    let items: Vec<Value> = media_groups
        .into_values()
        .map(|(media, files)| {
            json!({
                "media_item_id": media.id.to_string(),
                "kind": media.kind.as_str(),
                "title": media.title,
                "year": media.year,
                "files": files,
            })
        })
        .collect();
    ok_list(items).into_response()
}

/// GET /libraries/{id}/item-index — first-letter index buckets of owned items.
///
/// The buckets are a run-length encoding of the browse order, so they come from the same
/// SQL ordering the item page uses: one query, no in-memory selection.
pub(crate) async fn item_index(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lib_id = match id.parse::<domain::LibraryId>() {
        Ok(lib_id) => lib_id,
        Err(_) => return missing_library(),
    };
    {
        let store = state.store.lock();
        if let Err(response) = require_visible_library(&store, &id, user_id) {
            return response;
        }
    }
    let filter = match browse_filter_from_query(&query) {
        Ok(filter) => filter,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    let (order, descending) = browse_order_from_query(&query);
    let lib_q = library_query::LibraryQuery {
        library_id: lib_id,
        user_id,
        filter,
        order,
        descending,
        page: library_query::BrowsePage::Offset { start: 0, limit: 0 },
        include_total: false,
    };
    let titles = {
        let store = state.store.lock();
        match store.library_item_titles(&lib_q) {
            Ok(titles) => titles,
            Err(error) => return browse_store_error(error),
        }
    };
    let mut buckets: Vec<Value> = Vec::new();
    let mut current_initial = String::new();
    let mut current_offset = 0usize;
    let mut current_count = 0usize;
    for (index, title) in titles.iter().enumerate() {
        let initial = title
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "#".into());
        if initial != current_initial {
            if current_count > 0 {
                buckets.push(json!({
                    "initial": current_initial,
                    "count": current_count,
                    "offset": current_offset,
                }));
            }
            current_initial = initial;
            current_offset = index;
            current_count = 1;
        } else {
            current_count += 1;
        }
    }
    if current_count > 0 {
        buckets.push(json!({
            "initial": current_initial,
            "count": current_count,
            "offset": current_offset,
        }));
    }
    ok_list(buckets).into_response()
}

/// GET /libraries/{id}/facets — 十维筛选候选与「排除自身维」计数。
///
/// Dimension keys and labels are the frontend contract; counts come from the same predicate
/// builder the item page uses, with the dimension's own condition removed.
pub(crate) async fn facets(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // tier：缺失/空串按 all（旧调用方仍能拿到 resolutions）；其余非法值 400。
    let tier = match query.get("tier").map(String::as_str) {
        None | Some("") | Some("all") => library_query::FacetTier::All,
        Some("primary") => library_query::FacetTier::Primary,
        Some(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "library.invalid",
                "tier 必须是 primary 或 all",
            );
        }
    };
    let lib_id = match id.parse::<domain::LibraryId>() {
        Ok(lib_id) => lib_id,
        Err(_) => return missing_library(),
    };
    {
        let store = state.store.lock();
        if let Err(response) = require_visible_library(&store, &id, user_id) {
            return response;
        }
    }
    let filter = match browse_filter_from_query(&query) {
        Ok(filter) => filter,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    let request = library_query::FacetRequest {
        library_id: lib_id,
        user_id,
        filter,
        tier,
    };
    let facets = {
        let store = state.store.lock();
        match store.library_facets(&request) {
            Ok(facets) => facets,
            Err(error) => return browse_store_error(error),
        }
    };
    let mut body = serde_json::Map::new();
    body.insert("total".into(), json!(facets.total));
    for key in [
        "genres",
        "countries",
        "decades",
        "watch",
        "ratings",
        "runtimes",
        "languages",
        "resolutions",
        "hdr",
        "stock",
    ] {
        let counts = facets.groups.get(key).cloned().unwrap_or_default();
        match serde_json::to_value(counts) {
            Ok(value) => body.insert(key.into(), value),
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
    }
    // 兼容键：响应里一直有 `years`，保持空数组。
    body.insert("years".into(), json!([]));
    ok(Value::Object(body)).into_response()
}
