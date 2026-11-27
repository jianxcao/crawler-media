//! Per-season intro/outro diagnostics, standalone detection trigger, and manual CRUD.
use std::str::FromStr;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ScrapeStoreExt,
    http::{err, library::require_visible_library, ok},
    management::ApiState,
};

#[derive(Deserialize)]
pub(crate) struct SeasonMarkersQuery {
    season: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MarkerDetectMode {
    ReuseValid,
    Recapture,
    #[serde(rename = "theintrodb_only")]
    TheIntroDbOnly,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DetectMarkersRequest {
    season: u32,
    episode: Option<u32>,
    file_id: Option<domain::LedgerId>,
    mode: MarkerDetectMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutManualMarkerRequest {
    file_id: domain::LedgerId,
    season: u32,
    episode: u32,
    intro_start_ms: Option<i64>,
    intro_end_ms: Option<i64>,
    outro_start_ms: Option<i64>,
    outro_end_ms: Option<i64>,
    locked: bool,
}

#[derive(Deserialize)]
pub(crate) struct DeleteManualMarkerQuery {
    file_id: domain::LedgerId,
    season: u32,
    episode: u32,
}

pub(crate) async fn season_markers(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<SeasonMarkersQuery>,
) -> Response {
    let Ok(media_id) = domain::MediaId::from_str(&item_id) else {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效");
    };
    let season = query.season.unwrap_or(1);
    let probe = state.probe.clone();
    let loaded = super::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App],
        move |store| {
            let library = require_visible_library(store, &id, user_id)?;
            if store
                .get_media(media_id)
                .map_err(marker_store_error)?
                .is_none()
            {
                return Err(err(
                    StatusCode::NOT_FOUND,
                    "library.item_missing",
                    "条目不存在",
                ));
            }
            let lib_id = library
                .id
                .parse::<domain::LibraryId>()
                .map_err(|e| marker_store_error(e.into()))?;
            let rows = store
                .file_coverage_in_library_season(
                    lib_id,
                    media_id,
                    season,
                    store::FileReadScope::AllRecorded,
                )
                .map_err(marker_store_error)?;
            let episodes = build_episode_rows(store, &rows, season)?;
            let job = store
                .latest_marker_job_for_season(&media_id.to_string(), season)
                .map_err(marker_store_error)?;
            let active = job.as_ref().is_some_and(|j| j.is_active());
            let job_json = job
                .as_ref()
                .map(|j| probe_job_json_enriched(store, &probe, j));
            Ok(json!({
                "season": season,
                "active": active,
                "job": job_json,
                "episodes": episodes,
            }))
        },
    )
    .await;
    match loaded {
        Ok(Ok(payload)) => ok(payload).into_response(),
        Ok(Err(response)) => response,
        Err(error) => error.response(),
    }
}

pub(crate) async fn detect_markers(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Json(body): Json<DetectMarkersRequest>,
) -> Response {
    if let Err(response) = state
        .require_writable()
        .and_then(|()| state.require_write_admitted())
    {
        return response;
    }
    let Ok(media_id) = domain::MediaId::from_str(&item_id) else {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效");
    };
    let season = body.season;
    let factory = state.theintrodb_factory.clone();
    let target_ep = body.episode;
    let target_fid = body.file_id;
    let loaded = super::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App],
        move |store| {
            let library = require_visible_library(store, &id, None)?;
            let media = store
                .get_media(media_id)
                .map_err(marker_store_error)?
                .ok_or_else(|| err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"))?;
            if media.kind != domain::MediaKind::Tv {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "markers.unsupported_kind",
                    "仅电视作品支持分季/分集片头片尾检测",
                ));
            }
            let lib_id = library
                .id
                .parse::<domain::LibraryId>()
                .map_err(|e| marker_store_error(e.into()))?;
            let rows = store
                .file_coverage_in_library_season(
                    lib_id,
                    media_id,
                    season,
                    store::FileReadScope::AllRecorded,
                )
                .map_err(marker_store_error)?;
            let filtered = filter_target_rows(&rows, season, target_ep, target_fid);
            let theintrodb = crate::theintrodb_lookup::configured_client(store, factory.as_ref());
            Ok((media, filtered, theintrodb))
        },
    )
    .await;
    let (media, filtered, theintrodb) = match loaded {
        Ok(Ok(v)) => v,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let skipped_multi_unit = filtered.iter().filter(|r| r.units.len() != 1).count();
    let single_rows: Vec<domain::FileCoverage> = filtered
        .into_iter()
        .filter(|r| r.units.len() == 1)
        .collect();
    if single_rows.is_empty() {
        if skipped_multi_unit > 0 {
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "chapters.unit_time_mapping_unsupported",
                "多单元合并文件缺少分集时间映射，无法自动检测",
            );
        }
        return err(
            StatusCode::NOT_FOUND,
            "library.item_missing",
            "未找到可检测的在位单集文件",
        );
    }
    if body.mode == MarkerDetectMode::TheIntroDbOnly {
        for row in &single_rows {
            state.theintrodb_negative.invalidate(row.file.file_id);
            let _ = crate::marker_resolver::resolve_item_chapters(
                &state,
                row,
                row.units[0],
                &media,
                theintrodb.clone(),
            )
            .await;
        }
        return ok(json!({
            "queued": false,
            "already_running": false,
            "skipped_multi_unit": skipped_multi_unit,
            "job": Value::Null,
        }))
        .into_response();
    }
    enqueue_detect_work(&state, media, single_rows, season, skipped_multi_unit, body).await
}

async fn enqueue_detect_work(
    state: &ApiState,
    media: domain::Media,
    single_rows: Vec<domain::FileCoverage>,
    season: u32,
    skipped_multi_unit: usize,
    body: DetectMarkersRequest,
) -> Response {
    let reuse = body.mode == MarkerDetectMode::ReuseValid;
    let season_wide = body.episode.is_none() && body.file_id.is_none();
    let worker = state.clone();
    let outcome = state
        .http_work
        .run_to_completion(move || {
            super::store_wait::with_foreground_store(&worker, |store| {
                let (queued, already_running) = if season_wide {
                    let units = single_rows
                        .iter()
                        .map(|row| crate::probe_manager::ProbeUnit {
                            row: row.clone(),
                            kind: media.kind,
                            force_fingerprint: true,
                            reuse_fingerprint_cache: reuse,
                            overwrite_markers: true,
                            reuse_media_info_cache: true,
                            marker_refresh_id: None,
                            job_id: None,
                        })
                        .collect();
                    let policy = if reuse {
                        crate::fingerprint_job::CapturePolicy::ReuseValid
                    } else {
                        crate::fingerprint_job::CapturePolicy::Recapture
                    };
                    let (created, count) = worker
                        .probe
                        .enqueue_marker_refresh_job_with_policy(units, policy);
                    let active = single_rows
                        .iter()
                        .any(|r| worker.probe.is_queued(&r.file.file_id.to_string()));
                    (created || count > 0, !created && active)
                } else {
                    let mut any_queued = false;
                    let mut any_running = false;
                    for row in &single_rows {
                        let unit = crate::probe_manager::ProbeUnit {
                            row: row.clone(),
                            kind: media.kind,
                            force_fingerprint: true,
                            reuse_fingerprint_cache: reuse,
                            overwrite_markers: true,
                            reuse_media_info_cache: true,
                            marker_refresh_id: None,
                            job_id: None,
                        };
                        let (created, _) = worker
                            .probe
                            .enqueue_single_episode_marker_detect(unit, reuse)
                            .map_err(marker_store_error)?;
                        any_queued |= created;
                        any_running |=
                            !created && worker.probe.is_queued(&row.file.file_id.to_string());
                    }
                    (any_queued, !any_queued && any_running)
                };
                let job = store
                    .latest_marker_job_for_season(&media.id.to_string(), season)
                    .map_err(marker_store_error)?
                    .map(|j| probe_job_json_enriched(store, &worker.probe, &j));
                Ok::<_, Response>((queued, already_running, job))
            })?
        })
        .await;
    match outcome {
        Ok(Ok((queued, already_running, job))) => ok(json!({
            "queued": queued,
            "already_running": already_running,
            "skipped_multi_unit": skipped_multi_unit,
            "job": job,
        }))
        .into_response(),
        Ok(Err(response)) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

pub(crate) async fn put_manual_marker(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Json(body): Json<PutManualMarkerRequest>,
) -> Response {
    if let Err(message) = validate_manual_bounds(&body) {
        return err(StatusCode::BAD_REQUEST, "markers.invalid", message);
    }
    let Ok(media_id) = domain::MediaId::from_str(&item_id) else {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效");
    };
    let Ok(season_i32) = i32::try_from(body.season) else {
        return err(StatusCode::BAD_REQUEST, "markers.invalid", "季号无效");
    };
    let Ok(ep_i32) = i32::try_from(body.episode) else {
        return err(StatusCode::BAD_REQUEST, "markers.invalid", "集号无效");
    };
    let unit = domain::UnitKey::new(season_i32, ep_i32);
    let row = match load_owned_file_row(&state, id, media_id, body.file_id, unit).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    if row.units.len() != 1 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            "多单元合并文件不支持单集时间轴标记",
        );
    }
    if let Err(reason) = crate::fingerprint_job::source::observe_source(&row) {
        return err(StatusCode::CONFLICT, "chapters.source_changed", &reason);
    }
    let observation = store::SourceObservation {
        file_id: row.file.file_id,
        expected: row.file.version.clone(),
        presence: domain::FilePresence::Present,
    };
    let marker = store::StoredFileMarker {
        file_id: row.file.file_id,
        version: row.file.version.clone(),
        unit,
        intro_start_ms: body.intro_start_ms,
        intro_end_ms: body.intro_end_ms,
        outro_start_ms: body.outro_start_ms,
        outro_end_ms: body.outro_end_ms,
        source: "manual".into(),
        locked: body.locked,
        updated_at: 0,
    };
    let frames = state.chapter_frames.clone();
    let saved = super::store_wait::store_write(&state, move |store| {
        let file_id = marker.file_id;
        let unit = marker.unit;
        store.put_file_marker_checked(&marker, &[observation])?;
        store.get_file_marker(file_id, unit)
    })
    .await;
    match saved {
        Ok(Ok(Some(stored))) => {
            frames.notify_pending();
            ok(marker_json(&stored)).into_response()
        }
        Ok(Ok(None)) => err(
            StatusCode::CONFLICT,
            "chapters.source_changed",
            "源文件已变更",
        ),
        Ok(Err(error)) => marker_store_error(error),
        Err(response) => response,
    }
}

pub(crate) async fn delete_manual_marker(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<DeleteManualMarkerQuery>,
) -> Response {
    let Ok(media_id) = domain::MediaId::from_str(&item_id) else {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效");
    };
    let (Ok(s), Ok(e)) = (i32::try_from(query.season), i32::try_from(query.episode)) else {
        return err(StatusCode::BAD_REQUEST, "markers.invalid", "季集参数无效");
    };
    let unit = domain::UnitKey::new(s, e);
    if let Err(response) = load_owned_file_row(&state, id, media_id, query.file_id, unit).await {
        return response;
    }
    let frames = state.chapter_frames.clone();
    let file_id = query.file_id;
    match super::store_wait::store_write(&state, move |store| {
        store.delete_file_marker(file_id, unit)
    })
    .await
    {
        Ok(Ok(())) => {
            frames.notify_pending();
            ok(json!({ "deleted": true })).into_response()
        }
        Ok(Err(error)) => marker_store_error(error),
        Err(response) => response,
    }
}

async fn load_owned_file_row(
    state: &ApiState,
    lib_id_str: String,
    media_id: domain::MediaId,
    file_id: domain::LedgerId,
    unit: domain::UnitKey,
) -> Result<domain::FileCoverage, Response> {
    super::store_wait::store_read(state, &[store::Db::Library, store::Db::App], move |store| {
        let library = require_visible_library(store, &lib_id_str, None)?;
        let row = store
            .file_coverage_by_id(file_id)
            .map_err(marker_store_error)?
            .filter(|r| {
                r.file.library_id.to_string() == library.id
                    && r.file.media_id == media_id
                    && r.file.presence == domain::FilePresence::Present
                    && r.file.publication == domain::FilePublication::Published
                    && r.units.contains(&unit)
            })
            .ok_or_else(|| {
                err(
                    StatusCode::NOT_FOUND,
                    "library.item_missing",
                    "在位分集文件不存在",
                )
            })?;
        Ok(row)
    })
    .await
    .map_err(|e| e.response())?
}

fn validate_manual_bounds(body: &PutManualMarkerRequest) -> Result<(), &'static str> {
    let intro = match (body.intro_start_ms, body.intro_end_ms) {
        (Some(s), Some(e)) if s >= 0 && s < e => Some((s, e)),
        (None, None) => None,
        _ => return Err("片头起止时间必须同时填写且开始时间小于结束时间"),
    };
    let outro = match (body.outro_start_ms, body.outro_end_ms) {
        (Some(s), Some(e)) if s >= 0 && s < e => Some((s, e)),
        (None, None) => None,
        _ => return Err("片尾起止时间必须同时填写且开始时间小于结束时间"),
    };
    if intro.is_none() && outro.is_none() {
        return Err("请至少填写片头或片尾起止时间，清除标记请使用删除接口");
    }
    if let (Some((_, intro_end)), Some((outro_start, _))) = (intro, outro)
        && intro_end > outro_start
    {
        return Err("片头结束时间不能晚于片尾开始时间");
    }
    Ok(())
}

fn filter_target_rows(
    rows: &[domain::FileCoverage],
    season: u32,
    episode: Option<u32>,
    file_id: Option<domain::LedgerId>,
) -> Vec<domain::FileCoverage> {
    rows.iter()
        .filter(|row| {
            row.file.presence == domain::FilePresence::Present
                && row.file.publication == domain::FilePublication::Published
                && file_id.is_none_or(|fid| row.file.file_id == fid)
                && row.units.iter().any(|u| {
                    u.episode_coordinates().is_some_and(|(s, ep)| {
                        s == season && episode.is_none_or(|target_ep| ep == target_ep)
                    })
                })
        })
        .cloned()
        .collect()
}

fn build_episode_rows(
    store: &crate::Store,
    rows: &[domain::FileCoverage],
    season: u32,
) -> Result<Vec<Value>, Response> {
    let mut items = Vec::new();
    for row in rows {
        if row.file.presence != domain::FilePresence::Present
            || row.file.publication != domain::FilePublication::Published
        {
            continue;
        }
        let Some((unit, ep)) = row.units.iter().find_map(|u| {
            u.episode_coordinates()
                .filter(|(s, _)| *s == season)
                .map(|(_, ep)| (*u, ep))
        }) else {
            continue;
        };
        let fid_str = row.file.file_id.to_string();
        let multi_unit = row.units.len() != 1;
        let duration_ms = store
            .get_media_info_cache_version(&fid_str)
            .ok()
            .flatten()
            .and_then(|v| v.format_duration_ms)
            .or(row.file.duration_ms);
        let marker = if multi_unit {
            None
        } else {
            store
                .get_file_marker(row.file.file_id, unit)
                .map_err(marker_store_error)?
                .as_ref()
                .map(marker_json)
        };
        let detection = store
            .latest_probe_unit_for_ledger(&fid_str)
            .map_err(marker_store_error)?
            .map(|u| detection_json(&u, multi_unit));
        items.push((
            ep,
            fid_str.clone(),
            json!({
                "file_id": fid_str,
                "season": season,
                "episode": ep,
                "duration_ms": duration_ms,
                "multi_unit": multi_unit,
                "marker": marker,
                "detection": detection,
            }),
        ));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(items.into_iter().map(|(_, _, v)| v).collect())
}

fn marker_json(m: &store::StoredFileMarker) -> Value {
    json!({
        "intro_start_ms": m.intro_start_ms,
        "intro_end_ms": m.intro_end_ms,
        "outro_start_ms": m.outro_start_ms,
        "outro_end_ms": m.outro_end_ms,
        "source": m.source,
        "locked": m.locked,
        "updated_at": m.updated_at,
    })
}

fn detection_json(unit: &store::ProbeJobUnit, multi_unit: bool) -> Value {
    let parsed = unit
        .detection_outcome_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let kind_status = |key: &str| -> Option<Value> {
        if multi_unit {
            return Some(json!({
                "status": "unsupported",
                "decision": "unsupported",
                "reason": "unit_time_mapping_unsupported",
                "reason_label": "多集单文件暂不支持自动映射",
            }));
        }
        let k = parsed.as_ref()?.get("kinds")?.get(key)?;
        let status = k.get("status").and_then(Value::as_str).unwrap_or("unknown");
        let decision = k
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let reason = k.get("reason").and_then(Value::as_str);
        Some(json!({
            "status": status,
            "decision": decision,
            "reason": reason,
            "reason_label": reason_label_zh(status, decision, reason),
        }))
    };
    json!({
        "job_id": unit.job_id,
        "stage": unit.stage.as_str(),
        "status": unit.status,
        "error": unit.error,
        "intro": kind_status("intro"),
        "outro": kind_status("outro"),
    })
}

fn reason_label_zh(status: &str, decision: &str, reason: Option<&str>) -> &'static str {
    if status == "verified" || decision == "verified" {
        return "声纹共识已验证";
    }
    if status == "no_match" || decision == "no_match" {
        return "无匹配片段";
    }
    let Some(r) = reason else {
        return "待更多剧集验证";
    };
    if r.contains("insufficient_episodes") || r.contains("insufficient_independent") {
        "有效同季集数不足，无法达成共识"
    } else if r.contains("conflicting") {
        "边界证据存在冲突，保留原标记"
    } else if r.contains("ambiguous") {
        "存在多组相近重复声纹，暂不自动覆盖"
    } else if r.contains("sampling_limit") || r.contains("budget") {
        "采样窗口预算已达上限"
    } else if r.contains("incomplete") || r.contains("unusable") || r.contains("timeline") {
        "音频时间轴或采样不完整"
    } else {
        "声纹候选未决，保留当前标记"
    }
}

pub(crate) fn probe_job_json_enriched(
    store: &crate::Store,
    probe: &crate::probe_manager::ProbeManager,
    job: &store::ProbeJob,
) -> Value {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    let units = store.probe_job_units(&job.id).unwrap_or_default();
    let phase = if !job.is_active() {
        job.status.as_str()
    } else if units
        .iter()
        .any(|u| u.stage == store::ProbeDispatchStage::Analysis)
    {
        "analyzing"
    } else if units
        .iter()
        .any(|u| u.stage == store::ProbeDispatchStage::Fingerprint && u.status == "running")
    {
        "capturing"
    } else if units
        .iter()
        .any(|u| u.stage == store::ProbeDispatchStage::Metadata && u.status == "running")
    {
        "probing_metadata"
    } else {
        "queued"
    };
    let sampling_mode = units
        .iter()
        .find_map(|u| {
            let plan: Value = serde_json::from_str(u.sampling_plan_json.as_deref()?).ok()?;
            plan.get("sampling_mode")?.as_str().map(String::from)
        })
        .or_else(|| {
            store
                .get_scrape_config()
                .ok()
                .map(|c| c.effective.fingerprint_sampling_mode)
        });
    let timing = probe.timing_snapshot(&job.id);
    let input_bytes = timing.as_ref().and_then(|t| {
        let mut sum = 0u64;
        let mut any = false;
        for att in &t.capture_attempts {
            if let Some(b) = att.metrics.get("input_bytes").and_then(Value::as_u64) {
                sum = sum.saturating_add(b);
                any = true;
            }
        }
        any.then_some(sum)
    });
    json!({
        "id": job.id,
        "kind": job.kind,
        "season": job.season,
        "status": job.status,
        "total": job.total,
        "completed": job.completed,
        "succeeded": job.succeeded,
        "failed": job.failed,
        "error": job.error,
        "elapsed_ms": job.elapsed_ms(now_ms),
        "phase": phase,
        "sampling_mode": sampling_mode,
        "queue_wait_ms": timing.as_ref().and_then(|t| t.stage_totals.queue_wait),
        "priority_wait_ms": timing.as_ref().and_then(|t| t.stage_totals.remote_gate_wait),
        "metrics": {
            "input_bytes": input_bytes,
            "measurement_complete": timing.as_ref().is_some_and(|t| t.persisted_attempts_complete),
        },
    })
}

fn marker_store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "片头片尾状态或手动标记存取失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}
