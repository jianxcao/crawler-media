//! Direct TheIntroDB cloud synchronization (`mode: "theintrodb_only"`) for TV and Movie.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::{
    http::{err, ok},
    management::ApiState,
};

use super::{known_file_duration_ms, marker_store_error};

pub(super) struct CloudTarget {
    pub(super) row: domain::FileCoverage,
    pub(super) unit: domain::UnitKey,
    pub(super) duration_ms: Option<i64>,
    pub(super) locked: bool,
}

pub(super) fn prepare_cloud_targets(
    store: &crate::Store,
    media: &domain::Media,
    rows: &[domain::FileCoverage],
    season: Option<u32>,
) -> Result<Vec<CloudTarget>, Response> {
    let mut out = Vec::new();
    for row in rows.iter().filter(|r| r.units.len() == 1) {
        let unit = match media.kind {
            domain::MediaKind::Movie => domain::UnitKey::WHOLE,
            domain::MediaKind::Tv => {
                let target_season = season.unwrap_or(1);
                let Some(u) = row.units.iter().copied().find(|u| {
                    u.episode_coordinates()
                        .is_some_and(|(s, _)| s == target_season)
                }) else {
                    continue;
                };
                u
            }
            domain::MediaKind::Video => {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "markers.unsupported_kind",
                    "该媒体类型不支持片头片尾检测",
                ));
            }
        };
        let locked = store
            .get_file_marker(row.file.file_id, unit)
            .map_err(marker_store_error)?
            .is_some_and(|m| m.locked);
        out.push(CloudTarget {
            duration_ms: known_file_duration_ms(store, row),
            row: row.clone(),
            unit,
            locked,
        });
    }
    Ok(out)
}

pub(super) async fn run_theintrodb_only(
    state: &ApiState,
    media: domain::Media,
    targets: Vec<CloudTarget>,
    client: crate::theintrodb::TheIntroDbClient,
    skipped_multi_unit: usize,
) -> Response {
    let Some(tmdb_id) = media
        .tmdb_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
    else {
        return ok(json!({
            "queued": false,
            "already_running": false,
            "skipped_multi_unit": skipped_multi_unit,
            "updated_episodes": 0,
            "job": Value::Null,
        }))
        .into_response();
    };
    let reservation = match state
        .chapter_work
        .reserve(std::time::Duration::from_secs(5))
        .await
    {
        Ok(res) => res,
        Err(_) => return crate::http::store_wait::busy_response(),
    };
    let worker = state.clone();
    let result = reservation
        .run_to_completion(move || {
            let mut updated_episodes = 0usize;
            for target in targets {
                if target.locked {
                    continue;
                }
                worker
                    .theintrodb_negative
                    .invalidate(target.row.file.file_id);
                let now_ms =
                    crate::playback_events::PlaybackClock::now_ms(worker.playback_clock.as_ref());
                let Some(marker) = crate::theintrodb_lookup::lookup_cloud_marker_for_kind(
                    crate::theintrodb_lookup::CloudLookupRequest {
                        client: &client,
                        negative: worker.theintrodb_negative.as_ref(),
                        file_id: target.row.file.file_id,
                        version: &target.row.file.version,
                        unit: target.unit,
                        duration_ms: target.duration_ms,
                        media_title: &media.title,
                        tmdb_id: &tmdb_id,
                        now_ms,
                        force_refresh: true,
                    },
                    media.kind,
                ) else {
                    continue;
                };
                if publish_direct_cloud_marker(&worker, &media, &target, &marker) {
                    updated_episodes += 1;
                }
            }
            if updated_episodes > 0 {
                worker.chapter_frames.notify_pending();
            }
            updated_episodes
        })
        .await;
    match result {
        Ok(updated_episodes) => ok(json!({
            "queued": false,
            "already_running": false,
            "skipped_multi_unit": skipped_multi_unit,
            "updated_episodes": updated_episodes,
            "job": Value::Null,
        }))
        .into_response(),
        Err(error) => crate::http::library::browse_work_error(error),
    }
}

fn publish_direct_cloud_marker(
    state: &ApiState,
    media: &domain::Media,
    target: &CloudTarget,
    marker: &store::StoredFileMarker,
) -> bool {
    if crate::fingerprint_job::source::observe_source(&target.row).is_err() {
        return false;
    }
    let Ok(lease) = state
        .library_work()
        .acquire_bounded(&state.store, &target.row.file.library_id.to_string())
    else {
        return false;
    };
    let observation = store::SourceObservation {
        file_id: target.row.file.file_id,
        expected: target.row.file.version.clone(),
        presence: domain::FilePresence::Present,
    };
    let outcome = lease.publish(|| {
        Ok(crate::http::store_wait::with_foreground_store(
            state,
            |store| {
                let current_dur = store
                    .file_coverage_by_id(target.row.file.file_id)
                    .ok()
                    .flatten()
                    .and_then(|r| known_file_duration_ms(store, &r));
                if current_dur != target.duration_ms {
                    return Ok(false);
                }
                let current_media = store.get_media(media.id).ok().flatten();
                if current_media.is_none_or(|m| m.kind != media.kind || m.tmdb_id != media.tmdb_id)
                {
                    return Ok(false);
                }
                if store
                    .get_file_marker(target.row.file.file_id, target.unit)
                    .ok()
                    .flatten()
                    .is_some_and(|m| m.locked)
                {
                    return Ok(false);
                }
                store
                    .put_file_marker_checked(marker, &[observation])
                    .map(|()| true)
            },
        ))
    });
    matches!(outcome, Ok(Ok(Ok(true))))
}
