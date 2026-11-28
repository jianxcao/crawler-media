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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefreshChapters {
    capture_policy: crate::fingerprint_job::CapturePolicy,
}

/// Refresh chapters and queue a season replacement with an explicit capture policy.
/// Existing markers remain visible until the replacement is ready.
pub(crate) async fn refresh_item_chapters(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
    axum::Json(body): axum::Json<RefreshChapters>,
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
        ..
    } = match capture_target(&state, id, media_id, user_id, query).await {
        Ok(target) => target,
        Err(response) => return response,
    };

    let requested_season = unit
        .episode_coordinates()
        .map_or(1, |coordinates| coordinates.0);
    if row.units.len() != 1 {
        tracing::error!(%media_id, season = requested_season, error_kind = "unit_time_mapping_unsupported",
            "chapter refresh rejected: multiunit file needs unit time mapping");
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            "多单元文件缺少分集时间映射，保留现有片头片尾标记",
        );
    }

    state.theintrodb_negative.invalidate(row.file.file_id);
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
        .filter(|candidate| covers_season(candidate, season) && candidate.units.len() == 1)
        .cloned()
        .collect();
    let items = match chapter_items_off_executor(&state, row, refreshed).await {
        Ok(items) => items,
        Err(response) => return response,
    };
    finalize_chapter_refresh(
        &state,
        &media,
        &fingerprint_rows,
        season,
        body.capture_policy,
        items,
    )
    .await
}

async fn finalize_chapter_refresh(
    state: &ApiState,
    media: &domain::Media,
    fingerprint_rows: &[domain::FileCoverage],
    season: u32,
    capture_policy: crate::fingerprint_job::CapturePolicy,
    items: Vec<Value>,
) -> Response {
    let worker = state.clone();
    let media_id = media.id;
    let media = media.clone();
    let rows = fingerprint_rows.to_vec();
    let after = state
        .http_work
        .run_to_completion(move || {
            let outcome = crate::http::store_wait::with_foreground_store(&worker, |store| {
                let (job_created, probe_queued, season_probe_was_active, exact_job_id) =
                    enqueue_season_refresh(&worker, &media, &rows, season, capture_policy);
                let marker_job = match (exact_job_id, rows.first().map(|r| r.file.library_id)) {
                    (Some(jid), Some(lib_id)) => store
                        .marker_job_in_library(&jid, lib_id)
                        .map_err(chapter_store_error)?,
                    (None, Some(lib_id)) => store
                        .active_or_latest_marker_job_for_season(
                            lib_id,
                            &media_id.to_string(),
                            season,
                        )
                        .map_err(chapter_store_error)?,
                    _ => worker
                        .probe
                        .marker_refresh_job(media_id, season)
                        .map_err(chapter_store_error)?,
                };
                Ok::<_, Response>((
                    job_created,
                    probe_queued,
                    season_probe_was_active,
                    marker_job,
                ))
            })??;
            let (job_created, probe_queued, season_probe_was_active, marker_job) = outcome;
            let marker_job_active = marker_job.as_ref().is_some_and(|job| job.is_active());
            let active_after_enqueue = rows
                .iter()
                .any(|candidate| worker.probe.is_queued(&candidate.file.file_id.to_string()));
            let already_running = !job_created
                && probe_queued == 0
                && (marker_job_active || season_probe_was_active || active_after_enqueue);
            let enqueue_failed = should_queue_voiceprint_refresh(media.kind)
                && !job_created
                && probe_queued == 0
                && !already_running;
            Ok::<_, Response>((
                job_created,
                probe_queued,
                marker_job_active,
                already_running,
                enqueue_failed,
                marker_job,
            ))
        })
        .await;
    let (job_created, probe_queued, marker_job_active, already_running, enqueue_failed, marker_job) =
        match after {
            Ok(Ok(after)) => after,
            Ok(Err(response)) => return response,
            Err(error) => return crate::http::library::browse_work_error(error),
        };
    ok(json!({
        "chapters": items,
        "fingerprint_refresh_queued": job_created || probe_queued > 0 || marker_job_active,
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
    capture_policy: crate::fingerprint_job::CapturePolicy,
) -> (bool, usize, bool, Option<String>) {
    use crate::probe_manager::MarkerDetectEnqueue;

    let mut season_probe_was_active = !state.probe.fingerprint_dispatch_paused()
        && fingerprint_rows
            .iter()
            .any(|candidate| state.probe.is_queued(&candidate.file.file_id.to_string()));
    let mut job_created = false;
    let mut probe_queued = 0;
    let mut exact_job_id = None;
    if should_queue_voiceprint_refresh(media.kind)
        && !season_probe_was_active
        && let Some(lib_id) = fingerprint_rows.first().map(|r| r.file.library_id)
    {
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
        match state
            .probe
            .enqueue_library_marker_refresh(lib_id, units, capture_policy)
        {
            MarkerDetectEnqueue::Queued { job_id, dispatched } => {
                job_created = true;
                probe_queued = dispatched;
                exact_job_id = Some(job_id);
                tracing::info!(
                    media = %media.title,
                    season,
                    rows = fingerprint_rows.len(),
                    queued = probe_queued,
                    "【片头片尾】用户强制刷新，已排队同季完整声纹探测"
                );
            }
            MarkerDetectEnqueue::AlreadyRunning { job_id } => {
                season_probe_was_active = true;
                exact_job_id = Some(job_id);
            }
            MarkerDetectEnqueue::Conflict | MarkerDetectEnqueue::Failed(_) => {}
        }
    } else if season_probe_was_active {
        tracing::info!(
            media = %media.title,
            season,
            rows = fingerprint_rows.len(),
            "【片头片尾】该季已有探测任务，跳过重复刷新"
        );
    }

    (
        job_created,
        probe_queued,
        season_probe_was_active,
        exact_job_id,
    )
}

#[derive(serde::Deserialize)]
pub(crate) struct SeasonQuery {
    season: Option<u32>,
}

/// GET /libraries/{id}/items/{item_id}/probe-status?season=N — persistent
/// progress for the latest forced marker refresh in this season.
pub(crate) async fn probe_status(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<SeasonQuery>,
) -> Response {
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let season = query.season.unwrap_or(1);
    let loaded = crate::http::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App],
        move |store| {
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
                .file_coverage_in_library_season(
                    library_id,
                    media_id,
                    season,
                    store::FileReadScope::AllRecorded,
                )
                .map_err(chapter_store_error)?;
            let visible = rows.iter().any(|row| {
                covers_season(row, season) && row.file.library_id.to_string() == library.id
            });
            if !visible {
                return Err(err(
                    StatusCode::NOT_FOUND,
                    "library.item_missing",
                    "该季没有在位文件",
                ));
            }
            let job = store
                .latest_marker_refresh_for_library_season(library_id, &media_id.to_string(), season)
                .map_err(|error| {
                    tracing::error!(%error, %media_id, season, "读取片头片尾任务状态失败");
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "library.probe_status_failed",
                        "读取片头片尾任务状态失败，请稍后重试",
                    )
                })?;
            Ok(job)
        },
    )
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
