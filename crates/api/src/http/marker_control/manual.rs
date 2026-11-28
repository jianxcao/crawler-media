//! Manual intro/outro marker creation, unlocking, validation, and checked deletion.
use std::str::FromStr;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    http::{err, library::require_visible_library, ok, strict_json::StrictJson},
    management::ApiState,
};

use super::{
    DeleteManualMarkerQuery, PutManualMarkerRequest, known_file_duration_ms, marker_store_error,
    render::marker_json, resolve_unit_for_kind,
};

pub(crate) async fn put_manual_marker(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    StrictJson(body): StrictJson<PutManualMarkerRequest>,
) -> Response {
    if let Err(response) = state
        .require_writable()
        .and_then(|()| state.require_write_admitted())
    {
        return response;
    }
    if let Err(message) = validate_manual_bounds(&body, None) {
        return err(StatusCode::BAD_REQUEST, "markers.invalid", message);
    }
    let Ok(media_id) = domain::MediaId::from_str(&item_id) else {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效");
    };
    let (row, unit, duration_ms) = match load_owned_file_target(
        &state,
        id,
        media_id,
        body.file_id,
        body.season,
        body.episode,
    )
    .await
    {
        Ok(target) => target,
        Err(response) => return response,
    };
    if row.units.len() != 1 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            "多单元合并文件不支持单集时间轴标记",
        );
    }
    if let Err(message) = validate_manual_bounds(&body, duration_ms) {
        return err(StatusCode::BAD_REQUEST, "markers.invalid", message);
    }
    let file_id = row.file.file_id;
    let version = row.file.version.clone();
    if let Err(response) = observe_manual_source(&state, row).await {
        return response;
    }
    let observation = store::SourceObservation {
        file_id,
        expected: version.clone(),
        presence: domain::FilePresence::Present,
    };
    let marker = store::StoredFileMarker {
        file_id,
        version,
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
    let saved = crate::http::store_wait::store_write(&state, move |store| {
        let file_id = marker.file_id;
        let unit = marker.unit;
        if let Some(current) = store
            .file_coverage_by_id(file_id)
            .map_err(marker_store_error)?
            && let Some(cur_dur) = known_file_duration_ms(store, &current)
            && (marker.intro_end_ms.is_some_and(|end| end > cur_dur)
                || marker.outro_end_ms.is_some_and(|end| end > cur_dur))
        {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "markers.invalid",
                "片头或片尾结束时间不能超过媒体文件总时长",
            ));
        }
        let _ = store
            .put_file_marker_checked(&marker, &[observation])
            .map_err(marker_store_error)?;
        store
            .get_file_marker(file_id, unit)
            .map_err(marker_store_error)
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
            "源文件已变更，请刷新后重试",
        ),
        Ok(Err(response)) | Err(response) => response,
    }
}

pub(crate) async fn delete_manual_marker(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<DeleteManualMarkerQuery>,
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
    let (row, unit, _) = match load_owned_file_target(
        &state,
        id,
        media_id,
        query.file_id,
        query.season,
        query.episode,
    )
    .await
    {
        Ok(target) => target,
        Err(response) => return response,
    };
    if row.units.len() != 1 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            "多单元合并文件不支持单集时间轴标记",
        );
    }
    let observed_file_id = row.file.file_id;
    let observed_version = row.file.version.clone();
    if let Err(response) = observe_manual_source(&state, row).await {
        return response;
    }
    let observation = store::SourceObservation {
        file_id: observed_file_id,
        expected: observed_version,
        presence: domain::FilePresence::Present,
    };
    let frames = state.chapter_frames.clone();
    let file_id = query.file_id;
    match crate::http::store_wait::store_write(&state, move |store| {
        store.delete_file_marker_checked(file_id, unit, &[observation])
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

async fn observe_manual_source(
    state: &ApiState,
    row: domain::FileCoverage,
) -> Result<(), Response> {
    let observed = state
        .fs_work
        .run_to_completion(move || crate::fingerprint_job::source::observe_source(&row))
        .await;
    match observed {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(err(
            StatusCode::CONFLICT,
            "chapters.source_changed",
            "源文件已变更或不可访问，请刷新后重试",
        )),
        Err(crate::blocking_budget::BlockingError::Busy) => {
            Err(crate::http::store_wait::busy_response())
        }
        Err(error) => Err(crate::http::library::browse_work_error(error)),
    }
}

async fn load_owned_file_target(
    state: &ApiState,
    lib_id_str: String,
    media_id: domain::MediaId,
    file_id: domain::LedgerId,
    season: Option<u32>,
    episode: Option<u32>,
) -> Result<(domain::FileCoverage, domain::UnitKey, Option<i64>), Response> {
    crate::http::store_wait::store_read(
        state,
        &[store::Db::Library, store::Db::App],
        move |store| {
            let library = require_visible_library(store, &lib_id_str, None)?;
            let media = store
                .get_media(media_id)
                .map_err(marker_store_error)?
                .ok_or_else(|| err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"))?;
            let unit = resolve_unit_for_kind(media.kind, season, episode)?;
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
                        "在位媒体文件不存在",
                    )
                })?;
            let duration_ms = known_file_duration_ms(store, &row);
            Ok((row, unit, duration_ms))
        },
    )
    .await
    .map_err(|e| e.response())?
}

fn validate_manual_bounds(
    body: &PutManualMarkerRequest,
    duration_ms: Option<i64>,
) -> Result<(), &'static str> {
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
    if let Some(dur) = duration_ms.filter(|&d| d > 0) {
        if intro.is_some_and(|(_, end)| end > dur) {
            return Err("片头结束时间不能超过媒体文件总时长");
        }
        if outro.is_some_and(|(_, end)| end > dur) {
            return Err("片尾结束时间不能超过媒体文件总时长");
        }
    }
    Ok(())
}
