//! Forced marker refresh and persistent per-season job status.
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::str::FromStr;

use super::{
    ChapterQuery, ChapterTarget, capture_target, chapter_items_off_executor, chapter_store_error,
    covers_season, require_visible_library,
};
use crate::http::{err, ok};
use crate::management::ApiState;

/// POST /libraries/{id}/items/{item_id}/chapters/refresh — force clear cached
/// chapters and persistent markers, then re-resolve from TheIntroDB / ffprobe.
pub(crate) async fn refresh_item_chapters(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
) -> Response {
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let ChapterTarget {
        row,
        unit,
        media,
        rows,
        theintrodb: theintrodb_client,
    } = match capture_target(&state, id, media_id, user_id, query).await {
        Ok(target) => target,
        Err(response) => return response,
    };

    let requested_season = unit
        .episode_coordinates()
        .map_or(1, |coordinates| coordinates.0);
    if row.units.len() != 1
        || rows.iter().any(|candidate| {
            covers_season(candidate, requested_season) && candidate.units.len() != 1
        })
    {
        tracing::error!(%media_id, season = requested_season, error_kind = "unit_time_mapping_unsupported",
            "chapter refresh rejected: multiunit file needs unit time mapping");
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            "多单元文件缺少分集时间映射，保留现有片头片尾标记",
        );
    }

    let refreshed = if media.kind == domain::MediaKind::Tv {
        crate::marker_resolver::resolve_item_chapters(&state, &row, unit, &media, theintrodb_client)
            .await
    } else {
        match crate::marker_resolver::force_refresh_item_chapters(
            &state,
            &row,
            unit,
            &media,
            theintrodb_client,
        )
        .await
        {
            Ok(chapters) => chapters,
            Err(error) => return chapter_refresh_error(error),
        }
    };

    let season = unit
        .episode_coordinates()
        .map_or(1, |coordinates| coordinates.0);
    let fingerprint_rows: Vec<_> = rows
        .iter()
        .filter(|candidate| covers_season(candidate, season))
        .cloned()
        .collect();
    let items = match chapter_items_off_executor(&state, row, refreshed).await {
        Ok(items) => items,
        Err(response) => return response,
    };
    let worker = state.clone();
    let after = state
        .http_work
        .run_to_completion(move || {
            worker.require_writable()?;
            worker.require_write_admitted()?;
            let (probe_queued, season_probe_was_active) =
                enqueue_season_refresh(&worker, &media, &fingerprint_rows, season);
            let marker_job = worker
                .probe
                .marker_refresh_job(media_id, season)
                .map_err(chapter_store_error)?;
            let marker_job_active = marker_job.as_ref().is_some_and(|job| job.is_active());
            let active_after_enqueue = fingerprint_rows
                .iter()
                .any(|candidate| worker.probe.is_queued(&candidate.file.file_id.to_string()));
            let already_running = probe_queued == 0
                && (marker_job_active || season_probe_was_active || active_after_enqueue);
            let enqueue_failed = should_queue_voiceprint_refresh(media.kind)
                && probe_queued == 0
                && !already_running;
            Ok::<_, Response>((
                probe_queued,
                marker_job_active,
                already_running,
                enqueue_failed,
                marker_job,
            ))
        })
        .await;
    let (probe_queued, marker_job_active, already_running, enqueue_failed, marker_job) = match after
    {
        Ok(Ok(after)) => after,
        Ok(Err(response)) => return response,
        Err(error) => return crate::http::library::browse_work_error(error),
    };
    ok(json!({
        "chapters": items,
        "fingerprint_refresh_queued": probe_queued > 0 || marker_job_active,
        "fingerprint_refresh_already_running": already_running,
        "fingerprint_refresh_error": enqueue_failed.then_some("声纹刷新任务未能入队"),
        "fingerprint_refresh_job": marker_job.as_ref().map(probe_job_json),
    }))
    .into_response()
}

fn chapter_refresh_error(error: crate::marker_resolver::ChapterRefreshError) -> Response {
    use crate::marker_resolver::ChapterRefreshError;
    match error {
        ChapterRefreshError::UnitTimeMappingUnsupported => err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            &error.to_string(),
        ),
        ChapterRefreshError::Unavailable(status) => err(status, "runtime.busy", &error.to_string()),
        ChapterRefreshError::SourceChanged(message) => {
            err(StatusCode::CONFLICT, "chapters.source_changed", &message)
        }
        ChapterRefreshError::Storage(message) => {
            err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &message)
        }
    }
}

fn enqueue_season_refresh(
    state: &ApiState,
    media: &domain::Media,
    fingerprint_rows: &[domain::FileCoverage],
    season: u32,
) -> (usize, bool) {
    let season_probe_was_active = fingerprint_rows
        .iter()
        .any(|candidate| state.probe.is_queued(&candidate.file.file_id.to_string()));
    let mut probe_queued = 0;
    if should_queue_voiceprint_refresh(media.kind) && !season_probe_was_active {
        let units = fingerprint_rows
            .iter()
            .map(|candidate_row| crate::probe_manager::ProbeUnit {
                row: candidate_row.clone(),
                kind: media.kind,
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
                marker_refresh_id: None,
                job_id: None,
            })
            .collect();
        probe_queued = state.probe.enqueue_marker_refresh(units);
        tracing::info!(
            media = %media.title,
            season,
            rows = fingerprint_rows.len(),
            queued = probe_queued,
            "【片头片尾】用户强制刷新，已排队同季完整声纹探测"
        );
    } else if season_probe_was_active {
        tracing::info!(
            media = %media.title,
            season,
            rows = fingerprint_rows.len(),
            "【片头片尾】该剧该季已有探测任务，跳过重复刷新"
        );
    }

    (probe_queued, season_probe_was_active)
}

/// GET /libraries/{id}/items/{item_id}/probe-status?season=N — persistent
/// progress for the latest forced marker refresh in this season.
pub(crate) async fn probe_status(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
) -> Response {
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let season = query.season.unwrap_or(1);
    let loaded = crate::http::store_wait::store_read(&state, move |store| {
        let library = require_visible_library(store, &id, user_id)?;
        let Some(_media) = store.get_media(media_id).map_err(chapter_store_error)? else {
            return Err(err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "条目不存在",
            ));
        };
        let library_id = library
            .id
            .parse::<domain::LibraryId>()
            .map_err(|error| chapter_store_error(error.into()))?;
        let rows = store
            .file_coverage_in_library_season(library_id, media_id, season)
            .map_err(chapter_store_error)?;
        let visible = rows
            .iter()
            .any(|row| covers_season(row, season) && row.file.library_id.to_string() == library.id);
        if !visible {
            return Err(err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "该季没有在位文件",
            ));
        }
        let job = store
            .latest_marker_refresh_for_season(&media_id.to_string(), season)
            .map_err(|error| {
                tracing::error!(%error, %media_id, season, "读取片头片尾任务状态失败");
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "library.probe_status_failed",
                    "读取片头片尾任务状态失败，请稍后重试",
                )
            })?;
        Ok(job)
    })
    .await;
    let job = match loaded {
        Ok(Ok(job)) => job,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let active = job.as_ref().is_some_and(|job| job.is_active());
    ok(json!({
        "active": active,
        "job": job.as_ref().map(probe_job_json),
    }))
    .into_response()
}

fn probe_job_json(job: &crate::store::ProbeJob) -> Value {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
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
        "phase": Value::Null,
        "sampling_mode": Value::Null,
        "queue_wait_ms": Value::Null,
        "priority_wait_ms": Value::Null,
        "metrics": {
            "input_bytes": Value::Null,
            "measurement_complete": false,
        },
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn manual_tv_refresh_always_queues_voiceprint() {
        assert!(
            super::should_queue_voiceprint_refresh(domain::MediaKind::Tv),
            "manual TV refresh must queue a complete voiceprint pass regardless of prior markers"
        );
    }

    #[test]
    fn manual_movie_refresh_does_not_queue_tv_voiceprint() {
        assert!(!super::should_queue_voiceprint_refresh(
            domain::MediaKind::Movie
        ));
    }
}

fn should_queue_voiceprint_refresh(media_kind: domain::MediaKind) -> bool {
    media_kind == domain::MediaKind::Tv
}
