//! Per-season and per-movie intro/outro diagnostics, standalone detection, and manual CRUD.
#[path = "marker_control/cloud.rs"]
mod cloud;
#[path = "marker_control/detect.rs"]
mod detect;
#[path = "marker_control/manual.rs"]
mod manual;
#[path = "marker_control/render.rs"]
mod render;

use std::str::FromStr;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    http::{err, library::require_visible_library, ok},
    management::ApiState,
};

pub(crate) use detect::detect_markers;
pub(crate) use manual::{delete_manual_marker, put_manual_marker};
pub(crate) use render::probe_job_json_enriched;

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
    #[serde(default)]
    pub(super) season: Option<u32>,
    #[serde(default)]
    pub(super) episode: Option<u32>,
    #[serde(default)]
    pub(super) file_id: Option<domain::LedgerId>,
    pub(super) mode: MarkerDetectMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutManualMarkerRequest {
    pub(super) file_id: domain::LedgerId,
    #[serde(default)]
    pub(super) season: Option<u32>,
    #[serde(default)]
    pub(super) episode: Option<u32>,
    #[serde(default)]
    pub(super) intro_start_ms: Option<i64>,
    #[serde(default)]
    pub(super) intro_end_ms: Option<i64>,
    #[serde(default)]
    pub(super) outro_start_ms: Option<i64>,
    #[serde(default)]
    pub(super) outro_end_ms: Option<i64>,
    pub(super) locked: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteManualMarkerQuery {
    pub(super) file_id: domain::LedgerId,
    pub(super) season: Option<u32>,
    pub(super) episode: Option<u32>,
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
    let probe = state.probe.clone();
    let paused = probe.fingerprint_dispatch_paused();
    let loaded = super::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App],
        move |store| {
            let library = require_visible_library(store, &id, user_id)?;
            let Some(media) = store.get_media(media_id).map_err(marker_store_error)? else {
                return Err(err(
                    StatusCode::NOT_FOUND,
                    "library.item_missing",
                    "条目不存在",
                ));
            };
            let lib_id = library
                .id
                .parse::<domain::LibraryId>()
                .map_err(|e| marker_store_error(e.into()))?;
            match media.kind {
                domain::MediaKind::Tv => {
                    let season = query.season.unwrap_or(1);
                    let rows = store
                        .file_coverage_in_library_season(
                            lib_id,
                            media_id,
                            season,
                            store::FileReadScope::AllRecorded,
                        )
                        .map_err(marker_store_error)?;
                    let episodes = render::build_tv_episode_rows(store, &rows, season)?;
                    let job = store
                        .latest_marker_job_for_season(&media_id.to_string(), season)
                        .map_err(marker_store_error)?;
                    let active = job.as_ref().is_some_and(|j| j.is_active());
                    let job_json = job
                        .as_ref()
                        .map(|j| probe_job_json_enriched(store, &probe, j));
                    Ok(json!({
                        "media_id": media_id.to_string(),
                        "season": season,
                        "active": active,
                        "paused": paused,
                        "job": job_json,
                        "episodes": episodes,
                    }))
                }
                domain::MediaKind::Movie => {
                    if query.season.is_some() {
                        return Err(err(
                            StatusCode::BAD_REQUEST,
                            "markers.invalid",
                            "电影条目不支持分季查询参数",
                        ));
                    }
                    let rows = store
                        .file_coverage_in_library(
                            lib_id,
                            Some(media_id),
                            store::FileReadScope::AllRecorded,
                        )
                        .map_err(marker_store_error)?;
                    let episodes = render::build_movie_rows(store, &rows)?;
                    Ok(json!({
                        "media_id": media_id.to_string(),
                        "season": Value::Null,
                        "active": false,
                        "paused": paused,
                        "job": Value::Null,
                        "episodes": episodes,
                    }))
                }
                domain::MediaKind::Video => Err(err(
                    StatusCode::BAD_REQUEST,
                    "markers.unsupported_kind",
                    "该媒体类型不支持片头片尾标记",
                )),
            }
        },
    )
    .await;
    match loaded {
        Ok(Ok(payload)) => ok(payload).into_response(),
        Ok(Err(response)) => response,
        Err(error) => error.response(),
    }
}

pub(super) fn resolve_unit_for_kind(
    kind: domain::MediaKind,
    season: Option<u32>,
    episode: Option<u32>,
) -> Result<domain::UnitKey, Response> {
    match kind {
        domain::MediaKind::Movie => match (season, episode) {
            (None, None) => Ok(domain::UnitKey::WHOLE),
            _ => Err(err(
                StatusCode::BAD_REQUEST,
                "markers.invalid",
                "电影条目不支持季号与集号参数",
            )),
        },
        domain::MediaKind::Tv => match (season, episode) {
            (Some(s), Some(e)) => {
                let Ok(s_i32) = i32::try_from(s) else {
                    return Err(err(StatusCode::BAD_REQUEST, "markers.invalid", "季号无效"));
                };
                let Ok(e_i32) = i32::try_from(e) else {
                    return Err(err(StatusCode::BAD_REQUEST, "markers.invalid", "集号无效"));
                };
                Ok(domain::UnitKey::new(s_i32, e_i32))
            }
            _ => Err(err(
                StatusCode::BAD_REQUEST,
                "markers.invalid",
                "电视作品标记必须指定季号与集号",
            )),
        },
        domain::MediaKind::Video => Err(err(
            StatusCode::BAD_REQUEST,
            "markers.unsupported_kind",
            "该媒体类型不支持片头片尾标记",
        )),
    }
}

pub(super) fn known_file_duration_ms(
    store: &crate::Store,
    row: &domain::FileCoverage,
) -> Option<i64> {
    let fid_str = row.file.file_id.to_string();
    store
        .get_media_info_cache_version(&fid_str)
        .ok()
        .flatten()
        .filter(|v| v.source_version == row.file.version.source_version)
        .and_then(|v| v.format_duration_ms)
        .or(row.file.duration_ms)
        .filter(|&d| d > 0)
}

pub(super) fn marker_store_error(error: store::StoreError) -> Response {
    match &error {
        store::StoreError::Invalid(msg) if msg.contains("unit_time_mapping_unsupported") => err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            "多单元合并文件不支持单集时间轴标记",
        ),
        store::StoreError::Invalid(msg)
            if msg.contains("marker_source_changed") || msg.contains("source_changed") =>
        {
            err(
                StatusCode::CONFLICT,
                "chapters.source_changed",
                "源文件或覆盖范围已变更，请刷新后重试",
            )
        }
        store::StoreError::Missing(msg)
            if msg.contains("source_changed")
                || msg.contains("file_deleted")
                || msg.contains("source_unobserved")
                || msg.contains("source_unknown") =>
        {
            err(
                StatusCode::CONFLICT,
                "chapters.source_changed",
                "源文件已变更或不可访问，请刷新后重试",
            )
        }
        store::StoreError::Protected(_) => err(
            StatusCode::CONFLICT,
            "library.protected",
            "当前媒体库或条目正在变更中，请稍后重试",
        ),
        store::StoreError::WriteBusy => super::store_wait::busy_response(),
        _ => {
            tracing::error!(%error, "片头片尾状态或手动标记存取失败");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                "片头片尾标记存取失败，请稍后重试",
            )
        }
    }
}
