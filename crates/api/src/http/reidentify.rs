//! 重新识别：把条目的身份（TMDB 锚点/标题）改挂到正确作品，或把文件拆出
//! 为「非独立作品」（extras）。前端契约对齐 ReidentifyDialog / ClaimPanels。

use std::collections::{HashMap, HashSet};
use std::path::Path as FsPath;
use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{FileVersion, LedgerId, Media, MediaAlias, MediaId, MediaKind};
use serde_json::{Value, json};
use store::library_write::ReassignFiles;

use crate::blocking_budget::BlockingError;
use crate::http::library::{poster_path, preferred_row};
use crate::http::title_ref::resolve_title;
use crate::http::{err, ok};
use crate::management::ApiState;

/// Blocking-budget failure → HTTP response（上游/存储错误走各自原有路径）。
fn blocking_error(error: BlockingError) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

fn review_item(media: &domain::Media, poster_url: Option<String>) -> Value {
    json!({
        "media_item_id": media.id.to_string(),
        "tmdb_id": media.tmdb_id,
        "title": media.title,
        "year": media.year,
        "poster_url": poster_url,
    })
}

/// POST /libraries/{id}/items/{item_id}/reidentification-preview —
/// 当前身份 + 文件名解析出的搜索种子（前端据此给候选）。
pub(crate) async fn preview(
    State(state): State<ApiState>,
    Path((_id, item_id)): Path<(String, String)>,
) -> Response {
    let store = state.store.lock();
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let rows: Vec<domain::LedgerRow> = store
        .list_ledger()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.media_id == media_id)
        .collect();
    let search_seed = rows
        .iter()
        .filter_map(|row| {
            FsPath::new(&row.path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| release::parse(name).title)
        })
        .next()
        .unwrap_or_else(|| media.title.clone());
    let selected = preferred_row(&rows);
    let poster = selected.and_then(poster_path);
    let poster_url = selected
        .filter(|_| poster.is_some())
        .map(|row| crate::http::library::artwork_url("posters", row.id));
    let groups: Vec<Value> = if rows.is_empty() {
        Vec::new()
    } else {
        vec![json!({
            "key": media.id.to_string(),
            "outcome": {
                "media_item_id": media.id.to_string(),
                "tmdb_id": media.tmdb_id,
                "title": media.title,
                "year": media.year,
                "poster_url": poster_url.clone(),
                "source": "current",
                "same_as_current": true,
                "reason": null,
                "code": null,
                "candidates": [],
            },
            "file_ids": rows.iter().map(|row| row.id.to_string()).collect::<Vec<_>>(),
            "file_count": rows.len(),
            "total_size_bytes": 0,
            "sample_names": rows.iter().take(3).filter_map(|row| {
                FsPath::new(&row.path).file_name().and_then(|n| n.to_str()).map(str::to_string)
            }).collect::<Vec<_>>(),
        })]
    };
    ok(json!({
        "current": review_item(&media, poster_url),
        "movie": media.kind == MediaKind::Movie,
        "groups": groups,
        "skipped_missing": 0,
        "pinned_identity": false,
        "unreachable": false,
        "search_seed": search_seed,
    }))
    .into_response()
}

/// POST /libraries/{id}/items/{item_id}/reidentifications —
/// 把条目指定文件改挂到 `title_ref` 指向的作品并重刷元数据。
pub(crate) async fn reidentify(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run_to_completion(move || reidentify_inner(&state_for_work, &body))
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error),
    }
}

struct ParsedReidentifyRequest {
    files_to_reassign: Vec<(LedgerId, FileVersion)>,
    from_media_id: MediaId,
    rows_to_refresh: Vec<domain::LedgerRow>,
    source_revision: i64,
}

fn parse_file_targets(
    store: &store::Store,
    body: &Value,
) -> Result<ParsedReidentifyRequest, (StatusCode, &'static str, String)> {
    let raw_file_ids = body["file_ids"].as_array().ok_or((
        StatusCode::BAD_REQUEST,
        "reidentify.invalid",
        "缺少 file_ids".into(),
    ))?;
    if raw_file_ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "reidentify.invalid",
            "file_ids 不能为空".into(),
        ));
    }

    let mut parsed_ids = Vec::with_capacity(raw_file_ids.len());
    for item in raw_file_ids {
        let s = item.as_str().ok_or((
            StatusCode::BAD_REQUEST,
            "reidentify.invalid",
            "file_id 必须是字符串".into(),
        ))?;
        let clean = s.replace('-', "");
        let lid = LedgerId::from_str(&clean).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "reidentify.invalid",
                format!("无效的 file_id: {s}"),
            )
        })?;
        parsed_ids.push(lid);
    }

    let mut explicit_versions: HashMap<LedgerId, FileVersion> = HashMap::new();
    if let Some(files_arr) = body["files"].as_array() {
        for f in files_arr {
            if let (Some(fid_str), Some(rev)) = (f["file_id"].as_str(), f["revision"].as_i64()) {
                let clean = fid_str.replace('-', "");
                if let Ok(lid) = LedgerId::from_str(&clean) {
                    let s_ver = f["source_version"].as_str().unwrap_or("").to_string();
                    explicit_versions.insert(
                        lid,
                        FileVersion {
                            revision: rev,
                            source_version: s_ver,
                        },
                    );
                }
            }
        }
    }

    let mut from_media_id: Option<MediaId> = None;
    let mut files_to_reassign = Vec::with_capacity(parsed_ids.len());
    let mut rows_to_refresh = Vec::with_capacity(parsed_ids.len());

    for lid in parsed_ids {
        let clean = lid.to_string().replace('-', "");
        let row = store.get_ledger(&clean).ok().flatten().ok_or((
            StatusCode::NOT_FOUND,
            "library.file_missing",
            format!("文件不存在: {lid}"),
        ))?;

        match from_media_id {
            None => from_media_id = Some(row.media_id),
            Some(prev_mid) => {
                if prev_mid != row.media_id {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "reidentify.invalid",
                        "file_ids 跨多个条目，需分别调用".into(),
                    ));
                }
            }
        }

        let version = match explicit_versions.get(&lid) {
            Some(v) => v.clone(),
            None => store.get_file_version(lid).ok().flatten().ok_or((
                StatusCode::NOT_FOUND,
                "library.file_missing",
                format!("文件版本不可读取: {lid}"),
            ))?,
        };

        files_to_reassign.push((lid, version));
        rows_to_refresh.push(row);
    }

    let from_media_id = from_media_id.ok_or((
        StatusCode::BAD_REQUEST,
        "reidentify.invalid",
        "缺少有效文件".into(),
    ))?;

    let source_revision = store
        .media_revision(from_media_id)
        .ok()
        .flatten()
        .unwrap_or(0);

    Ok(ParsedReidentifyRequest {
        files_to_reassign,
        from_media_id,
        rows_to_refresh,
        source_revision,
    })
}

fn reidentify_inner(state: &ApiState, body: &Value) -> Response {
    let parsed = {
        let store = state.store.lock();
        match parse_file_targets(&store, body) {
            Ok(p) => p,
            Err((status, code, msg)) => return err(status, code, &msg),
        }
    };

    let Some(title_ref) = body["title_ref"].as_str() else {
        return err(
            StatusCode::BAD_REQUEST,
            "reidentify.invalid",
            "缺少 title_ref",
        );
    };

    let target_media = match resolve_title(state, title_ref) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return err(
                StatusCode::NOT_FOUND,
                "reidentify.no_match",
                "没有匹配到作品",
            );
        }
        Err(e) => return err(StatusCode::BAD_GATEWAY, "reidentify.upstream", &e),
    };

    let _from_media = match state.store.lock().get_media(parsed.from_media_id) {
        Ok(Some(m)) => m,
        _ => {
            return err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "当前条目不存在",
            );
        }
    };

    let target_id = target_media.tmdb_id.as_deref().unwrap_or("");
    if !target_id.is_empty() {
        let store = state.store.lock();
        if let Ok(Some(existing_media)) =
            store.get_media_by_alias_kind("tmdb_id", target_id, Some(target_media.kind))
        {
            if existing_media.id == parsed.from_media_id
                && existing_media.title == target_media.title
                && existing_media.year == target_media.year
            {
                // 已在当前 Media 就位，不迁移，返回 claimed = 0
                return ok(json!({ "claimed": 0 })).into_response();
            } else if existing_media.id != parsed.from_media_id {
                // 被另一个 Media 占有
                return err(
                    StatusCode::CONFLICT,
                    "reidentify.alias_conflict",
                    &format!(
                        "目标已被作品 {} 占有；跨条目身份合并请调用 POST /media/merges",
                        existing_media.id
                    ),
                );
            }
        }
    }

    let to_media = {
        let store = state.store.lock();
        match store.ensure_media(target_media.clone()) {
            Ok(m) => m,
            Err(e) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &e.to_string(),
                );
            }
        }
    };

    let reassign_req = ReassignFiles {
        files: parsed.files_to_reassign,
        from: parsed.from_media_id,
        to: to_media.id,
        expected_media_revision: parsed.source_revision,
    };

    let now_ms = crate::job_loop::unix_now() * 1000;
    let claimed = match state.store.lock().reassign_files(&reassign_req, now_ms) {
        Ok(c) => c,
        Err(store::StoreError::Protected(msg)) => {
            return err(StatusCode::CONFLICT, "library.protected", &msg);
        }
        Err(e) => return err(StatusCode::CONFLICT, "library.frozen", &e.to_string()),
    };

    if let Some(ext_id) = target_media.tmdb_id.as_deref() {
        let alias = MediaAlias {
            source: "tmdb".into(),
            kind: target_media.kind,
            external_id: ext_id.to_string(),
        };
        if let Err(e) = state.store.lock().add_media_alias(to_media.id, &alias) {
            match e {
                store::StoreError::Protected(msg) => {
                    return err(StatusCode::CONFLICT, "reidentify.alias_conflict", &msg);
                }
                _ => tracing::warn!(%e, "无法为目标媒体添加别名"),
            }
        }
    }

    // 针对已迁移的文件执行元数据刷新（失败不破坏已完成的迁移）
    refresh_sidecars_for_reassigned(state, &to_media, &parsed.rows_to_refresh);

    ok(json!({ "claimed": claimed })).into_response()
}

fn refresh_sidecars_for_reassigned(state: &ApiState, target: &Media, rows: &[domain::LedgerRow]) {
    let Some(row) = rows.first() else { return };
    let path = FsPath::new(&row.path).to_path_buf();
    let _ = crate::poster_fetch::attach_poster(state, target, &path);
    let _ = crate::poster_fetch::attach_backdrop(state, target, &path);

    if let Some(tmdb_id) = target.tmdb_id.clone() {
        match crate::scrape_metadata::fetch_tmdb_metadata(state, target.kind, &tmdb_id) {
            Ok(Some(meta)) => {
                let nfo = crate::scrape_metadata::nfo_from_tmdb(target, &meta);
                if target.kind == MediaKind::Tv {
                    crate::scrape_metadata::scrape_tv_sidecars(
                        state,
                        target,
                        &crate::scrape_metadata::show_root(&path, row),
                        rows,
                    );
                } else if let Some(stem) = path.file_stem().and_then(|v| v.to_str()) {
                    let nfo_path = path.with_file_name(format!("{stem}.nfo"));
                    crate::scrape_metadata::write_nfo(&nfo_path, target, &nfo);
                    let movie_root = path.parent().unwrap_or(&path);
                    crate::scrape_metadata::scrape_movie_fanart(state, target, movie_root);
                }
            }
            Ok(None) => {
                tracing::debug!(%tmdb_id, "catalog returned no metadata during reidentification")
            }
            Err(e) => {
                tracing::warn!(%e, %tmdb_id, "failed to scrape metadata during reidentification")
            }
        }
    }
}

/// POST /libraries/{id}/items/{item_id}/reidentifications/extras —
/// 把条目文件标为「非独立作品」：从台账拆出进未识别清单。
pub(crate) async fn extras(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let store = state.store.lock();
    let target_ids: HashSet<String> = body["file_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.replace('-', ""))
                .collect()
        })
        .unwrap_or_default();
    if target_ids.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "reidentify.invalid",
            "缺少 file_ids",
        );
    }
    let mut detached = 0usize;
    for file_id in target_ids {
        if let Some(row) = store.get_ledger(&file_id).ok().flatten() {
            if store
                .insert_unidentified(row.path.clone(), row.confidence)
                .is_ok()
                && store.delete_ledger_path(&row.path).is_ok()
            {
                detached += 1;
            }
        }
    }
    ok(json!({ "detached": detached })).into_response()
}
