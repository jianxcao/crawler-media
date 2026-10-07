//! Device-scoped Playback heartbeats.
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::{row_for, watch_state_json};
use crate::store::{SessionRow, UNIT_WHOLE, UnitState};
use crate::{
    http::{err, ok},
    job_loop::unix_now,
    management::ApiState,
};

struct Heartbeat<'a> {
    body: &'a Value,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    duration_ms: Option<i64>,
    now: i64,
}

pub(crate) async fn progress(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let device_id = body["device_id"].as_str().unwrap_or_default();
    if device_id.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "缺少 device_id",
        );
    }
    let (row, media, season, episode, duration_ms) = {
        let store = state.store.lock();
        match store.is_device_revoked(user_id, device_id) {
            Ok(true) => return err(StatusCode::FORBIDDEN, "playback.revoked", "该设备已被注销"),
            Ok(false) => {}
            Err(e) => {
                tracing::error!(%user_id, device_id, error = %e, "查询设备注销状态失败");
                return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", "查询设备状态失败");
            }
        }
        let Some((row, media, season, episode)) = row_for(&store, &body, Some(user_id)) else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let reported_duration = body["duration_ms"].as_i64().filter(|d| *d > 0);
        let duration =
            reported_duration.or_else(|| unit_duration(&store, user_id, &row, season, episode));
        (row, media, season, episode, duration)
    };
    if duration_ms.is_none() {
        let _ = crate::http::library::ensure_probe_enqueued(&state, &row);
    }
    let heartbeat = Heartbeat {
        body: &body,
        user_id,
        media_id: media.id,
        season,
        episode,
        duration_ms,
        now: unix_now(),
    };
    apply_heartbeat(&state.store.lock(), &heartbeat, device_id)
}

fn unit_duration(
    store: &crate::Store,
    user: domain::UserId,
    row: &domain::LedgerRow,
    season: i32,
    episode: i32,
) -> Option<i64> {
    store
        .unit_state(user, row.media_id, season, episode)
        .ok()
        .flatten()
        .and_then(|unit| unit.duration_ms)
        .or_else(|| {
            store
                .get_file_meta(&row.id.to_string())
                .ok()
                .flatten()
                .and_then(|tracks| tracks.video.and_then(|video| video.duration_secs))
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .map(|seconds| (seconds * 1000.0) as i64)
        })
}

fn update_unit(store: &crate::Store, heartbeat: &Heartbeat<'_>, event: &str) -> UnitState {
    if (heartbeat.season, heartbeat.episode) == (UNIT_WHOLE, UNIT_WHOLE) {
        if let Err(error) =
            store.copy_legacy_movie_unit_if_missing(heartbeat.user_id, heartbeat.media_id)
        {
            // Without the copy a fresh canonical row would shadow the legacy
            // state, so skip this write instead of losing watch history.
            tracing::error!(%error, media_id = %heartbeat.media_id, "迁移历史电影播放单元失败，跳过本次规范写入");
            let position = heartbeat.body["position_ms"].as_i64().unwrap_or(0);
            return UnitState {
                position_ms: position,
                played: false,
                favorite: false,
                duration_ms: heartbeat.duration_ms,
                audio_track: heartbeat.body["audio_track"].as_str().map(str::to_string),
                subtitle_track: heartbeat.body["subtitle_track"]
                    .as_str()
                    .map(str::to_string),
                play_count: 0,
                updated_at: heartbeat.now,
            };
        }
    }
    let existing = store
        .unit_state(
            heartbeat.user_id,
            heartbeat.media_id,
            heartbeat.season,
            heartbeat.episode,
        )
        .ok()
        .flatten();
    // start 事件如果未带 position_ms（如前端起播阶段汇报），保留数据库已存进度，避免被 0 覆盖
    let position = heartbeat.body["position_ms"]
        .as_i64()
        .or_else(|| {
            (event == "start")
                .then(|| existing.as_ref().map(|u| u.position_ms))
                .flatten()
        })
        .unwrap_or(0);
    let audio = heartbeat.body["audio_track"].as_str();
    let subtitle = heartbeat.body["subtitle_track"].as_str();
    // 播完（进度达到 90% 或离结尾不足 30 秒）时自动置 played=true，未达阈值时保留既有 played 状态
    let completed = heartbeat
        .duration_ms
        .is_some_and(|d| d > 0 && (position >= d * 9 / 10 || d - position <= 30_000));
    let played_update = if completed {
        Some(true)
    } else {
        existing.as_ref().and_then(|u| u.played.then_some(true))
    };
    store
        .upsert_unit(
            heartbeat.user_id,
            heartbeat.media_id,
            heartbeat.season,
            heartbeat.episode,
            position,
            played_update,
            None,
            heartbeat.duration_ms,
            audio,
            subtitle,
            event == "start",
            heartbeat.now,
        )
        .unwrap_or_else(|error| {
            tracing::error!(%error, media_id = %heartbeat.media_id, "Playback watch update failed");
            UnitState {
                position_ms: position,
                played: false,
                favorite: false,
                duration_ms: heartbeat.duration_ms,
                audio_track: audio.map(str::to_string),
                subtitle_track: subtitle.map(str::to_string),
                play_count: 0,
                updated_at: heartbeat.now,
            }
        })
}

fn next_session(
    heartbeat: &Heartbeat<'_>,
    device: &str,
    previous: Option<&SessionRow>,
    start: bool,
) -> SessionRow {
    let body = heartbeat.body;
    let position_ms = body["position_ms"].as_i64().unwrap_or(0);
    let paused = body["paused"].as_bool().unwrap_or(false);
    let previous = previous.filter(|_| !start);
    let delta = (position_ms - previous.map(|s| s.position_ms).unwrap_or(position_ms)).max(0);
    SessionRow {
        device_id: device.to_string(),
        user_id: heartbeat.user_id,
        media_id: heartbeat.media_id,
        season: (heartbeat.season != UNIT_WHOLE).then_some(heartbeat.season),
        episode: (heartbeat.episode != UNIT_WHOLE).then_some(heartbeat.episode),
        client: body["client"].as_str().map(str::to_string),
        device_name: body["device_name"].as_str().map(str::to_string),
        client_version: body["client_version"].as_str().map(str::to_string),
        play_method: "local".into(),
        position_ms,
        start_position_ms: previous.map(|s| s.start_position_ms).unwrap_or(position_ms),
        duration_ms: heartbeat.duration_ms,
        paused,
        watched_ms: previous.map(|s| s.watched_ms).unwrap_or(0) + if paused { 0 } else { delta },
        rate_bps: 0,
        bytes_sent: 0,
        connections: 1,
        admin_ended: previous.is_some_and(|s| s.admin_ended),
        started_at: previous.map(|s| s.started_at).unwrap_or(heartbeat.now),
        last_report_at: heartbeat.now,
    }
}

fn apply_heartbeat(store: &crate::Store, heartbeat: &Heartbeat<'_>, device: &str) -> Response {
    let event = heartbeat.body["event"].as_str().unwrap_or("progress");
    let previous = store.get_session(heartbeat.user_id, device).ok().flatten();
    let unit = update_unit(store, heartbeat, event);
    if previous.is_none() && event != "start" {
        // 会话已被结束（如管理员踢出或已关闭），拒绝隐式复活！
        return ok(watch_state_json(&unit, true)).into_response();
    }
    let next = next_session(heartbeat, device, previous.as_ref(), event == "start");
    let admin_ended = next.admin_ended && event != "stop";
    let update = if event == "stop" {
        previous.map(|mut closing| {
            closing.watched_ms = next.watched_ms;
            closing.position_ms = next.position_ms;
            closing
        })
    } else if admin_ended {
        None
    } else {
        Some(next)
    };
    if let Some(update) = update {
        if let Err(error) = store.upsert_session(&update) {
            tracing::error!(%error, device_id = device, "Playback session update failed");
        }
    }
    if event == "stop" || admin_ended {
        if let Err(error) = store.close_session(heartbeat.user_id, device, heartbeat.now) {
            tracing::error!(%error, device_id = device, "Playback session close failed");
        }
    }
    ok(watch_state_json(&unit, admin_ended)).into_response()
}
