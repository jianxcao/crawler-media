//! Standalone intro/outro detection (`reuse_valid`, `recapture`, and `theintrodb_only`).
use std::str::FromStr;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    http::{err, library::require_visible_library, ok, strict_json::StrictJson},
    management::ApiState,
};

use super::{
    DetectMarkersRequest, MarkerDetectMode,
    cloud::{prepare_cloud_targets, run_theintrodb_only},
    marker_store_error, probe_job_json_enriched,
};

pub(crate) async fn detect_markers(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    StrictJson(body): StrictJson<DetectMarkersRequest>,
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
    let factory = state.theintrodb_factory.clone();
    let req_season = body.season;
    let req_ep = body.episode;
    let req_fid = body.file_id;
    let req_mode = body.mode;
    let loaded = crate::http::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App],
        move |store| {
            let library = require_visible_library(store, &id, None)?;
            let media = store
                .get_media(media_id)
                .map_err(marker_store_error)?
                .ok_or_else(|| err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"))?;
            let lib_id = library
                .id
                .parse::<domain::LibraryId>()
                .map_err(|e| marker_store_error(e.into()))?;
            if store
                .library_item_detail(lib_id, media_id, "")
                .map_err(marker_store_error)?
                .is_none()
            {
                return Err(err(
                    StatusCode::NOT_FOUND,
                    "library.item_missing",
                    "条目不存在",
                ));
            }
            let filtered =
                select_detect_rows(store, &media, lib_id, req_season, req_ep, req_fid, req_mode)?;
            let theintrodb = if req_mode == MarkerDetectMode::TheIntroDbOnly {
                let Some(client) =
                    crate::theintrodb_lookup::configured_client(store, factory.as_ref())
                else {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        "markers.theintrodb_disabled",
                        "尚未在刮削设置中启用 TheIntroDB 云端查询",
                    ));
                };
                Some(client)
            } else {
                None
            };
            let cloud_targets = prepare_cloud_targets(store, &media, &filtered, req_season)?;
            Ok((lib_id, media, filtered, cloud_targets, theintrodb))
        },
    )
    .await;
    let (lib_id, media, filtered, cloud_targets, theintrodb) = match loaded {
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
            "未找到可检测的在位文件",
        );
    }
    if let Some(client) = theintrodb {
        return run_theintrodb_only(&state, media, cloud_targets, client, skipped_multi_unit).await;
    }
    let Some(season) = body.season else {
        return err(
            StatusCode::BAD_REQUEST,
            "markers.invalid",
            "电视作品检测必须指定季号",
        );
    };
    enqueue_detect_work(
        &state,
        lib_id,
        media,
        single_rows,
        season,
        skipped_multi_unit,
        body,
    )
    .await
}

fn select_detect_rows(
    store: &crate::Store,
    media: &domain::Media,
    lib_id: domain::LibraryId,
    season: Option<u32>,
    episode: Option<u32>,
    file_id: Option<domain::LedgerId>,
    mode: MarkerDetectMode,
) -> Result<Vec<domain::FileCoverage>, Response> {
    match media.kind {
        domain::MediaKind::Tv => {
            let Some(s) = season else {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "markers.invalid",
                    "电视作品检测必须指定季号",
                ));
            };
            let rows = store
                .file_coverage_in_library_season(
                    lib_id,
                    media.id,
                    s,
                    store::FileReadScope::AllRecorded,
                )
                .map_err(marker_store_error)?;
            Ok(filter_tv_rows(&rows, s, episode, file_id))
        }
        domain::MediaKind::Movie => {
            if mode != MarkerDetectMode::TheIntroDbOnly {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "markers.unsupported_kind",
                    "电影条目仅支持 TheIntroDB 云端同步，不支持同季声纹比对",
                ));
            }
            if season.is_some() || episode.is_some() {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "markers.invalid",
                    "电影条目检测不支持季号或集号参数",
                ));
            }
            let rows = store
                .file_coverage_in_library(lib_id, Some(media.id), store::FileReadScope::AllRecorded)
                .map_err(marker_store_error)?;
            Ok(rows
                .into_iter()
                .filter(|r| {
                    r.file.presence == domain::FilePresence::Present
                        && r.file.publication == domain::FilePublication::Published
                        && file_id.is_none_or(|fid| r.file.file_id == fid)
                        && r.units.contains(&domain::UnitKey::WHOLE)
                })
                .collect())
        }
        domain::MediaKind::Video => Err(err(
            StatusCode::BAD_REQUEST,
            "markers.unsupported_kind",
            "该媒体类型不支持片头片尾检测",
        )),
    }
}

fn filter_tv_rows(
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

async fn enqueue_detect_work(
    state: &ApiState,
    lib_id: domain::LibraryId,
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
            crate::http::store_wait::with_foreground_store(&worker, |store| {
                let (queued, already_running) =
                    enqueue_rows_on_probe(&worker, &media, &single_rows, reuse, season_wide)?;
                let job = store
                    .latest_marker_job_for_season(lib_id, &media.id.to_string(), season)
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
        Err(error) => crate::http::library::browse_work_error(error),
    }
}

fn enqueue_rows_on_probe(
    state: &ApiState,
    media: &domain::Media,
    single_rows: &[domain::FileCoverage],
    reuse: bool,
    season_wide: bool,
) -> Result<(bool, bool), Response> {
    if season_wide {
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
        let (created, count) = state
            .probe
            .enqueue_marker_refresh_job_with_policy(units, policy);
        let active = single_rows
            .iter()
            .any(|r| state.probe.is_queued(&r.file.file_id.to_string()));
        return Ok((created || count > 0, !created && active));
    }
    let mut any_queued = false;
    let mut any_running = false;
    for row in single_rows {
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
        let (created, _) = state
            .probe
            .enqueue_single_episode_marker_detect(unit, reuse)
            .map_err(marker_store_error)?;
        any_queued |= created;
        any_running |= !created && state.probe.is_queued(&row.file.file_id.to_string());
    }
    Ok((any_queued, !any_queued && any_running))
}
