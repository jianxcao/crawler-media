//! 重新识别：把条目的身份（TMDB 锚点/标题）改挂到正确作品，或把文件拆出
//! 为「非独立作品」（extras）。前端契约对齐 ReidentifyDialog / ClaimPanels。

use std::collections::HashSet;
use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{FileVersion, LedgerId, Media, MediaAlias, MediaId, MediaKind};
use serde_json::{Value, json};
use store::library_write::ReassignFiles;

use crate::blocking_budget::BlockingError;
use crate::http::title_ref::{invalid_title_ref, parse_title_ref, resolve_title};
use crate::http::{err, ok};
use crate::library_lifecycle::LibraryWorkLease;
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

mod preview;
pub(crate) use preview::preview;

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
    rows_to_refresh: Vec<domain::FileCoverage>,
    source_revision: i64,
}

/// The only accepted reidentification body: every file is pinned to the
/// version the client previewed, and the source Media to its revision.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReidentifyRequest {
    media_item_id: String,
    source_revision: i64,
    title_ref: String,
    files: Vec<ReidentifyFile>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReidentifyFile {
    file_id: String,
    revision: i64,
    source_version: String,
}

type ParseError = (StatusCode, &'static str, String);

fn invalid(msg: impl Into<String>) -> ParseError {
    (StatusCode::BAD_REQUEST, "reidentify.invalid", msg.into())
}

fn parse_pinned_files(
    request: &ReidentifyRequest,
) -> Result<Vec<(LedgerId, FileVersion)>, ParseError> {
    if request.files.is_empty() {
        return Err(invalid("files 不能为空"));
    }
    if request.source_revision < 0 {
        return Err(invalid("source_revision 无效"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(request.files.len());
    for file in &request.files {
        let id = LedgerId::from_str(&file.file_id.replace('-', ""))
            .map_err(|_| invalid(format!("无效的 file_id: {}", file.file_id)))?;
        if !seen.insert(id) {
            return Err(invalid(format!("重复的 file_id: {}", file.file_id)));
        }
        if file.revision <= 0 || file.source_version.is_empty() {
            return Err(invalid(format!("file {} 版本无效", file.file_id)));
        }
        out.push((
            id,
            FileVersion {
                revision: file.revision,
                source_version: file.source_version.clone(),
            },
        ));
    }
    Ok(out)
}

/// Validate the pinned request against current rows. Versions are never
/// filled in from the server: a stale pin fails the write CAS with 409.
fn parse_file_targets(
    store: &store::Store,
    request: &ReidentifyRequest,
) -> Result<ParsedReidentifyRequest, ParseError> {
    let from_media_id =
        MediaId::from_str(&request.media_item_id).map_err(|_| invalid("media_item_id 无效"))?;
    let files_to_reassign = parse_pinned_files(request)?;
    let mut rows_to_refresh = Vec::with_capacity(files_to_reassign.len());
    for (lid, _) in &files_to_reassign {
        let coverage = store
            .file_coverage_by_id(*lid)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    e.to_string(),
                )
            })?
            .ok_or((
                StatusCode::NOT_FOUND,
                "library.file_missing",
                format!("文件不存在: {lid}"),
            ))?;
        if coverage.file.media_id != from_media_id {
            return Err(invalid(format!("文件 {lid} 不属于条目 {from_media_id}")));
        }
        rows_to_refresh.push(coverage);
    }
    Ok(ParsedReidentifyRequest {
        files_to_reassign,
        from_media_id,
        rows_to_refresh,
        source_revision: request.source_revision,
    })
}

/// 冻结/删除中的 Library 必须拒绝重新识别：在任何目录读取、来源解析（catalog IO）
/// 或台账写入之前取得目标 Library 的工作租约。
///
/// 这两条路由没有 `{id}` 捕获，Library 只能由文件反查；租约因此与
/// `POST /libraries/{id}/organize` 共用同一套 `library_accepts_work` / 重配置冻结判定。
fn acquire_file_leases(
    state: &ApiState,
    file_ids: &[LedgerId],
) -> Result<Vec<LibraryWorkLease>, Response> {
    let library_ids = super::store_wait::with_bounded_store(state.store(), |store| {
        let mut ids = HashSet::new();
        for file_id in file_ids {
            if let Some((_, library_id)) =
                store.get_file_path_and_library(*file_id).map_err(|error| {
                    tracing::error!(%error, %file_id, "Reidentify lease context read failed");
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &error.to_string(),
                    )
                })?
            {
                ids.insert(library_id);
            }
        }
        Ok::<_, Response>(ids)
    })
    .ok_or_else(super::store_wait::busy_response)??;
    let mut leases = Vec::with_capacity(library_ids.len());
    for library_id in &library_ids {
        match state.library_work.acquire_bounded(&state.store, library_id) {
            Ok(lease) => leases.push(lease),
            Err(
                crate::library_lifecycle::BoundedLeaseError::RegistryBusy
                | crate::library_lifecycle::BoundedLeaseError::StoreBusy,
            ) => return Err(super::store_wait::busy_response()),
            Err(error) => {
                tracing::info!(%library_id, %error, "Library 冻结或删除中，拒绝重新识别写操作");
                return Err(err(
                    StatusCode::CONFLICT,
                    "library.frozen",
                    &error.to_string(),
                ));
            }
        }
    }
    Ok(leases)
}

fn reidentify_inner(state: &ApiState, body: &Value) -> Response {
    let request: ReidentifyRequest = match serde_json::from_value(body.clone()) {
        Ok(request) => request,
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                "reidentify.invalid",
                &format!("请求不符合契约: {error}"),
            );
        }
    };
    // Strict reference first: a rejected title_ref takes no lease and no Store work.
    let title = match parse_title_ref(&request.title_ref) {
        Ok(title) => title,
        Err(reason) => return invalid_title_ref(&reason),
    };
    let parsed = {
        let store = state.store.lock();
        match parse_file_targets(&store, &request) {
            Ok(p) => p,
            Err((status, code, msg)) => return err(status, code, &msg),
        }
    };

    // 冻结/删除中的 Library 只做拒绝，绝不做来源解析或目录 IO。
    let file_ids: Vec<LedgerId> = parsed.files_to_reassign.iter().map(|(id, _)| *id).collect();
    let _leases = match acquire_file_leases(state, &file_ids) {
        Ok(leases) => leases,
        Err(response) => return response,
    };

    let target_media = match resolve_title(state, &title) {
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
            store.get_media_by_alias_kind("tmdb", target_id, Some(target_media.kind))
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

    reassign_parsed_files(state, parsed, &target_media, &to_media)
}

/// 执行迁移并落别名与 sidecar 刷新副作用（失败不破坏已完成的迁移）。
///
/// `target` 是请求解析出的目标媒体（Tmdb 别名事实）；`to_media` 是
/// `ensure_media` 落库后的目标媒体。台账迁移与 sidecar 刷新都用 `to_media`，
/// 与拆分前 `reidentify_inner` 的行为完全一致。
fn reassign_parsed_files(
    state: &ApiState,
    parsed: ParsedReidentifyRequest,
    target: &Media,
    to_media: &Media,
) -> Response {
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

    if let Some(ext_id) = target.tmdb_id.as_deref() {
        let alias = MediaAlias {
            source: "tmdb".into(),
            kind: target.kind,
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
    refresh_sidecars_for_reassigned(state, to_media, &parsed.rows_to_refresh);

    ok(json!({ "claimed": claimed })).into_response()
}

fn refresh_sidecars_for_reassigned(
    state: &ApiState,
    target: &Media,
    coverages: &[domain::FileCoverage],
) {
    let Some(coverage) = coverages.first() else {
        return;
    };
    let path = coverage.file.path.clone();
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
                        &crate::scrape_metadata::show_root(&path, coverage),
                        coverages,
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

mod extras;
pub(crate) use extras::extras;
