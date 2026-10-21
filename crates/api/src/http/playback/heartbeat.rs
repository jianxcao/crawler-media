//! Device-scoped Playback heartbeats.
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::{effective_watch_state, row_for, watch_state_json};
use crate::store::{SessionRow, StoreError, UNIT_WHOLE, UnitState};
use crate::{
    http::{err, ok},
    job_loop::unix_now,
    management::ApiState,
};

struct Heartbeat<'a> {
    body: &'a Value,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    kind: domain::MediaKind,
    ledger_id: String,
    season: i32,
    episode: i32,
    duration_ms: Option<i64>,
    now: i64,
    now_ms: i64,
    source: domain::PhysicalSourceId,
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
    if let Some(rate_val) = body.get("rate") {
        if let Some(r) = rate_val.as_f64() {
            if !r.is_finite() || r < 0.25 || r > 4.0 {
                return err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid_rate",
                    "播放倍率必须在 0.25 到 4.0 之间",
                );
            }
        }
    }
    let play_session_id = body["play_session_id"].as_str().map(str::to_string);
    let event_sequence = body["event_sequence"].as_i64();
    if let Some(ref sid) = play_session_id {
        if sid.len() > 128 {
            return err(StatusCode::BAD_REQUEST, "playback.invalid", "play_session_id 超长");
        }
    }
    if let Some(seq) = event_sequence {
        if seq < 0 {
            return err(StatusCode::BAD_REQUEST, "playback.invalid", "event_sequence 必须非负");
        }
    }
    let (row, media, season, episode, duration_ms, source) = {
        let store = state.store.lock();
        match store.is_device_revoked(user_id, device_id) {
            Ok(true) => return err(StatusCode::FORBIDDEN, "playback.revoked", "该设备已被注销"),
            Ok(false) => {}
            Err(error) => return persistence_error(&error, user_id, device_id),
        }

        // If client provides a valid play_session_id, read target and source directly from the issued binding
        let binding = if let Some(ref sid) = play_session_id {
            store.play_session_binding(user_id, device_id, sid).ok().flatten()
        } else {
            None
        };

        if let Some(b) = binding {
            let Some(media) = store.get_media(b.media_id).ok().flatten() else {
                return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
            };
            let Some(source) = store.visible_file(user_id, b.ledger_id).ok().flatten() else {
                return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
            };
            let unit = (b.unit.season, b.unit.episode);
            let row = crate::media_server_provider::items::row_view(&source, unit);
            let duration = match unit_duration(&store, user_id, &row, unit.0, unit.1) {
                Ok(duration) => body["duration_ms"].as_i64().filter(|d| *d > 0).or(duration),
                Err(error) => return persistence_error(&error, user_id, device_id),
            };
            (
                row,
                media,
                unit.0,
                unit.1,
                duration,
                domain::PhysicalSourceId {
                    file_id: source.file_id,
                    revision: source.version.revision,
                },
            )
        } else {
            let Some((row, media, season, episode)) = row_for(&store, &body, Some(user_id)) else {
                return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
            };
            let Some(source) = store.visible_file(user_id, row.id).ok().flatten() else {
                return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
            };
            let duration = match unit_duration(&store, user_id, &row, season, episode) {
                Ok(duration) => body["duration_ms"].as_i64().filter(|d| *d > 0).or(duration),
                Err(error) => return persistence_error(&error, user_id, device_id),
            };
            (
                row,
                media,
                season,
                episode,
                duration,
                domain::PhysicalSourceId {
                    file_id: source.file_id,
                    revision: source.version.revision,
                },
            )
        }
    };
    if duration_ms.is_none() {
        let _ = crate::http::library::ensure_probe_enqueued(&state, &row);
    }
    let now_ms = state.playback_clock.now_ms();
    let heartbeat = Heartbeat {
        body: &body,
        user_id,
        media_id: media.id,
        kind: media.kind,
        ledger_id: row.id.to_string(),
        season,
        episode,
        duration_ms,
        now: now_ms / 1000,
        now_ms,
        source,
    };
    apply_heartbeat(&state.store.lock(), &heartbeat, device_id)
}

fn persistence_error(error: &StoreError, user_id: domain::UserId, device_id: &str) -> Response {
    tracing::error!(%error, %user_id, device_id, "Playback persistence failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        "保存播放状态失败",
    )
}

fn unit_duration(
    store: &crate::Store,
    user: domain::UserId,
    row: &domain::LedgerRow,
    season: i32,
    episode: i32,
) -> Result<Option<i64>, StoreError> {
    if let Some(duration) = effective_watch_state(store, user, row.media_id, season, episode)?
        .and_then(|unit| unit.duration_ms)
    {
        return Ok(Some(duration));
    }
    Ok(store
        .get_file_meta(&row.id.to_string())?
        .and_then(|tracks| tracks.video.and_then(|video| video.duration_secs))
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0) as i64))
}

fn update_unit(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    event: &str,
) -> Result<UnitState, StoreError> {
    // canonical whole-media coordinate (-1, -1) used directly
    let existing = store.unit_state(
        heartbeat.user_id,
        heartbeat.media_id,
        heartbeat.season,
        heartbeat.episode,
    )?;
    // A start with no position preserves the saved resume point.
    let position = heartbeat.body["position_ms"]
        .as_i64()
        .or_else(|| {
            (event == "start")
                .then(|| existing.as_ref().map(|u| u.position_ms))
                .flatten()
        })
        .unwrap_or(0)
        .max(0);
    let duration = heartbeat
        .duration_ms
        .or(existing.as_ref().and_then(|u| u.duration_ms));
    let completed = UnitState::playback_completed(position, duration);
    store.upsert_unit_for_source(
        heartbeat.user_id,
        heartbeat.media_id,
        domain::UnitKey::new(heartbeat.season, heartbeat.episode),
        heartbeat.source.clone(),
        position,
        completed.then_some(true),
        None,
        duration,
        heartbeat.body["audio_track"].as_str(),
        heartbeat.body["subtitle_track"].as_str(),
        event == "start",
        heartbeat.now,
    )
}

fn next_session(
    heartbeat: &Heartbeat<'_>,
    unit: &UnitState,
    device: &str,
    previous: Option<&SessionRow>,
    start: bool,
) -> SessionRow {
    let body = heartbeat.body;
    let position_ms = unit.position_ms;
    let paused = body["paused"].as_bool().unwrap_or(false);
    let previous = previous.filter(|_| !start);
    let rate = body["rate"].as_f64().unwrap_or(1.0);
    let now_ms = heartbeat.now_ms;
    let delta = previous
        .map(|prev| crate::playback_events::watched_increment(prev, position_ms, paused, false, now_ms, rate))
        .unwrap_or(0);
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
        duration_ms: unit.duration_ms,
        paused,
        watched_ms: previous.map(|s| s.watched_ms).unwrap_or(0).saturating_add(delta),
        rate_bps: 0,
        bytes_sent: 0,
        connections: 1,
        admin_ended: previous.is_some_and(|s| s.admin_ended),
        started_at: previous.map(|s| s.started_at).unwrap_or(heartbeat.now),
        last_report_at: heartbeat.now,
        ledger_id: Some(heartbeat.ledger_id.clone()),
        play_session_id: previous.and_then(|s| s.play_session_id.clone()),
        last_report_at_ms: heartbeat.now_ms,
        last_event_sequence: previous.and_then(|s| s.last_event_sequence),
    }
}

fn session_matches_heartbeat(session: &SessionRow, heartbeat: &Heartbeat<'_>) -> bool {
    session.media_id == heartbeat.media_id
        && session.season.unwrap_or(UNIT_WHOLE) == heartbeat.season
        && session.episode.unwrap_or(UNIT_WHOLE) == heartbeat.episode
}

fn heartbeat_matches_row(heartbeat: &Heartbeat<'_>, row: &domain::LedgerRow) -> bool {
    row.media_id == heartbeat.media_id
        && row.season.map(|value| value as i32) == (heartbeat.season != UNIT_WHOLE).then_some(heartbeat.season)
        && row.episode.map(|value| value as i32) == (heartbeat.episode != UNIT_WHOLE).then_some(heartbeat.episode)
}

fn persist_heartbeat(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    device: &str,
    resolved_session_id: Option<&str>,
) -> Result<(UnitState, bool), StoreError> {
    let ledger_id = match heartbeat.ledger_id.parse::<domain::LedgerId>() {
        Ok(lid) => lid,
        Err(_) => return Ok((store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?.unwrap_or_default(), true)),
    };

    // The file may have changed or does not cover the requested unit.
    // Use coverage membership instead of get_ledger LIMIT 1.
    let covers = store.file_covers_unit(
        ledger_id,
        heartbeat.media_id,
        domain::UnitKey::new(heartbeat.season, heartbeat.episode),
    )?;
    if !covers {
        tracing::warn!(ledger_id = %heartbeat.ledger_id, media_id = %heartbeat.media_id,
            "Ignoring stale Playback heartbeat because file does not cover unit");
        return Ok((
            store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                .unwrap_or_default(),
            true,
        ));
    }

    if let Some(owner) = store.file_owner_library(ledger_id)? {
        if !store.library_accepts_work(&owner.id)? {
            return Ok((
                store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                    .unwrap_or_default(),
                true,
            ));
        }
    }

    let event = heartbeat.body["event"].as_str().unwrap_or("progress");
    let previous = store.get_session(heartbeat.user_id, device)?;
    let play_session_id = heartbeat.body["play_session_id"].as_str().map(str::to_string);
    let event_sequence = heartbeat.body["event_sequence"].as_i64();

    let binding = if let Some(ref sid) = play_session_id {
        store.play_session_binding(heartbeat.user_id, device, sid)?
    } else {
        None
    };

    let identity = crate::playback_events::PlaybackEventIdentity {
        play_session_id: play_session_id.clone(),
        event_sequence,
    };

    let event_kind = match event {
        "start" => crate::playback_events::PlaybackEventKind::Start,
        "stop" => crate::playback_events::PlaybackEventKind::Stop,
        _ => crate::playback_events::PlaybackEventKind::Progress,
    };

    let ledger_id = match heartbeat.ledger_id.parse::<domain::LedgerId>() {
        Ok(lid) => lid,
        Err(_) => return Ok((store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?.unwrap_or_default(), true)),
    };

    let was_admin_ended = if previous.is_none() && event != "start" {
        store
            .device_was_admin_ended(heartbeat.user_id, device, heartbeat.now, 10)
            .unwrap_or(false)
    } else {
        false
    };

    if was_admin_ended {
        return Ok((
            store
                .unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                .unwrap_or_default(),
            true,
        ));
    }

    let session_id = match play_session_id {
        Some(sid) => sid,
        None => match resolved_session_id {
            Some(sid) => sid.to_string(),
            None => {
                let admin_ended = previous.as_ref().is_some_and(|p| p.admin_ended);
                return Ok((
                    store
                        .unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                        .unwrap_or_default(),
                    admin_ended,
                ));
            }
        },
    };

    let Some(b) = store.play_session_binding(heartbeat.user_id, device, &session_id)? else {
        let admin_ended = previous.as_ref().is_some_and(|p| p.admin_ended);
        return Ok((
            store
                .unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                .unwrap_or_default(),
            admin_ended,
        ));
    };

    let item = if heartbeat.season == UNIT_WHOLE {
        domain::TypedItemId::Work(heartbeat.media_id)
    } else {
        domain::TypedItemId::Unit {
            media: heartbeat.media_id,
            key: domain::UnitKey::new(heartbeat.season, heartbeat.episode),
        }
    };

    let event_type = match event {
        "start" => store::playback::PlaybackEventType::Playing,
        "stop" => store::playback::PlaybackEventType::Stopped,
        _ => store::playback::PlaybackEventType::Progress,
    };

    let existing = store.unit_state(
        heartbeat.user_id,
        heartbeat.media_id,
        heartbeat.season,
        heartbeat.episode,
    )?;
    let position_ms = heartbeat.body["position_ms"]
        .as_i64()
        .or_else(|| {
            (event == "start")
                .then(|| existing.as_ref().map(|u| u.position_ms))
                .flatten()
        })
        .unwrap_or(0)
        .max(0);
    let duration_ms = heartbeat
        .duration_ms
        .or(existing.as_ref().and_then(|u| u.duration_ms));

    let bound_event = store::playback::BoundPlaybackEvent {
        user_id: heartbeat.user_id,
        device_id: device.to_string(),
        session_id: session_id.clone(),
        generation: b.generation,
        event_sequence,
        item,
        source_id: heartbeat.source.clone(),
        source_version: b.source_version.unwrap_or_default(),
        position_ms,
        duration_ms,
        audio_track: heartbeat.body["audio_track"].as_str().map(str::to_string),
        subtitle_track: heartbeat.body["subtitle_track"].as_str().map(str::to_string),
        paused: heartbeat.body["paused"].as_bool().unwrap_or(false),
        rate_bps: 0,
        client: heartbeat.body["client"].as_str().map(str::to_string),
        event_type,
    };

    let _outcome = store.commit_playback_event(&bound_event, heartbeat.now_ms)?;
    let unit = store
        .unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
        .unwrap_or_default();
    let current_session = store.get_session(heartbeat.user_id, device)?;
    let admin_ended = current_session.as_ref().is_some_and(|s| s.admin_ended) && event != "stop";

    Ok((unit, admin_ended))
}

fn apply_heartbeat(store: &crate::Store, heartbeat: &Heartbeat<'_>, device: &str) -> Response {
    // Resolve a session id for legacy clients before opening the playback write
    // transaction: issuing opens its own immediate transaction and must not nest.
    let resolved_session_id = if heartbeat.body["play_session_id"].as_str().is_some() {
        None
    } else {
        let reusable = store
            .get_session(heartbeat.user_id, device)
            .ok()
            .flatten()
            .filter(|session| session_matches_heartbeat(session, heartbeat))
            .and_then(|session| session.play_session_id);
        match reusable {
            Some(sid) => Some(sid),
            None => heartbeat
                .ledger_id
                .parse::<domain::LedgerId>()
                .ok()
                .and_then(|ledger_id| {
                    store.visible_file(heartbeat.user_id, ledger_id).ok().flatten().and_then(|file| {
                        store.issue_play_session(
                            heartbeat.user_id, device, ledger_id, heartbeat.media_id,
                            domain::UnitKey::new(heartbeat.season, heartbeat.episode),
                            &file.version, heartbeat.now,
                        ).ok()
                    })
                }),
        }
    };
    match store.playback_write(|store| {
        persist_heartbeat(store, heartbeat, device, resolved_session_id.as_deref())
    }) {
        Ok((unit, ended)) => ok(watch_state_json(&unit, ended)).into_response(),
        Err(error) => persistence_error(&error, heartbeat.user_id, device),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_heartbeat_after_canonical_rebind_does_not_write_old_media() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(temp.path()).unwrap();
        let library = store.list_libraries().unwrap().into_iter()
            .find(|library| library.kind == domain::MediaKind::Movie).unwrap();
        let provisional = domain::Media { id: domain::MediaId::new(), kind: domain::MediaKind::Movie,
            title: "provisional".into(), year: None, original_title: None, aliases: Vec::new(), tmdb_id: None,
            douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None };
        let canonical = domain::Media { id: domain::MediaId::new(), kind: domain::MediaKind::Movie,
            title: "canonical".into(), year: None, original_title: None, aliases: Vec::new(), tmdb_id: Some("123".into()),
            douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None };
        store.insert_media(&provisional).unwrap();
        store.insert_media(&canonical).unwrap();
        let ledger = domain::LedgerRow { id: domain::LedgerId::new(), media_id: provisional.id,
            path: library.root_paths[0].join("movie.mkv").display().to_string(), season: None, episode: None,
            resolution: None, codec: None, hdr: None, quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High, filter_score: None };
        store.insert_ledger(&ledger).unwrap();
        store.rebind_scanned_ledger(ledger.id, canonical.id).unwrap();
        let user_id = domain::UserId::new();
        let body = serde_json::json!({"event":"start", "position_ms":500, "device_id":"race"});
        let file = store
            .published_files(&[ledger.id])
            .unwrap()
            .remove(&ledger.id)
            .expect("rebound fixture file is published");
        let heartbeat = Heartbeat { body: &body, user_id, media_id: provisional.id,
            kind: domain::MediaKind::Movie, ledger_id: ledger.id.to_string(),
            season: UNIT_WHOLE, episode: UNIT_WHOLE, duration_ms: Some(1000), now: 10, now_ms: 10_000,
            source: domain::PhysicalSourceId { file_id: file.file_id, revision: file.version.revision } };

        let (_, ended) = persist_heartbeat(&store, &heartbeat, "race", None).unwrap();

        assert!(ended);
        assert!(store.unit_state(user_id, provisional.id, UNIT_WHOLE, UNIT_WHOLE).unwrap().is_none());
        assert!(store.get_session(user_id, "race").unwrap().is_none());
    }
}
