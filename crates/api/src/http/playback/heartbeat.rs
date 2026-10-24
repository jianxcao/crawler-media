//! Device-scoped Playback heartbeats.
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::heartbeat_input::HeartbeatInput;
use super::{effective_watch_state, row_for, watch_state_json};
use crate::http::strict_json::StrictJson;
use crate::store::{StoreError, UNIT_WHOLE, UnitState};
use crate::{
    http::{err, ok},
    job_loop::unix_now,
    management::ApiState,
};

struct Heartbeat<'a> {
    body: &'a Value,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    ledger_id: domain::LedgerId,
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
    StrictJson(input): StrictJson<HeartbeatInput>,
) -> Response {
    if let Err(message) = input.validate() {
        tracing::warn!(message, "Playback heartbeat rejected");
        return err(StatusCode::BAD_REQUEST, "playback.invalid", message);
    }
    let body = match serde_json::to_value(input) {
        Ok(body) => body,
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                &error.to_string(),
            );
        }
    };
    let device_id = body["device_id"].as_str().unwrap_or_default();
    let (file, media, season, episode, duration_ms, source) = {
        let store = state.store.lock();
        match store.is_device_revoked(user_id, device_id) {
            Ok(true) => return err(StatusCode::FORBIDDEN, "playback.revoked", "该设备已被注销"),
            Ok(false) => {}
            Err(error) => return persistence_error(&error, user_id, device_id),
        }
        let session_id = body["play_session_id"].as_str().unwrap_or_default();
        match store.play_session_binding(user_id, device_id, session_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                tracing::warn!(%user_id, device_id, "Playback heartbeat rejected: unissued session identity");
                return err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid",
                    "play_session_id 未由此用户和设备签发",
                );
            }
            Err(error) => return persistence_error(&error, user_id, device_id),
        }
        let (file, media, season, episode) = match row_for(&store, &body, Some(user_id)) {
            Ok(row) => row,
            Err(error) => return error.response(),
        };
        let Some(source) = store.visible_file(user_id, file.file_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let duration = match unit_duration(&store, user_id, &file, season, episode) {
            Ok(duration) => body["duration_ms"].as_i64().filter(|d| *d > 0).or(duration),
            Err(error) => return persistence_error(&error, user_id, device_id),
        };
        (
            file,
            media,
            season,
            episode,
            duration,
            domain::PhysicalSourceId {
                file_id: source.file_id,
                revision: source.version.revision,
            },
        )
    };
    if duration_ms.is_none() {
        let coverage = domain::FileCoverage {
            path_key: String::new(),
            file: file.clone(),
            units: vec![domain::UnitKey::new(season, episode)],
        };
        let _ = crate::http::library::ensure_probe_enqueued(&state, &coverage);
    }
    let now_ms = state.playback_clock.now_ms();
    let heartbeat = Heartbeat {
        body: &body,
        user_id,
        media_id: media.id,
        ledger_id: file.file_id,
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
    file: &domain::LibraryFile,
    season: i32,
    episode: i32,
) -> Result<Option<i64>, StoreError> {
    if let Some(duration) = effective_watch_state(store, user, file.media_id, season, episode)?
        .and_then(|unit| unit.duration_ms)
    {
        return Ok(Some(duration));
    }
    Ok(store
        .get_file_meta(&file.file_id.to_string())?
        .and_then(|tracks| tracks.video.and_then(|video| video.duration_secs))
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0) as i64))
}

fn current_unit(store: &crate::Store, heartbeat: &Heartbeat<'_>) -> Result<UnitState, StoreError> {
    Ok(store
        .unit_state(
            heartbeat.user_id,
            heartbeat.media_id,
            heartbeat.season,
            heartbeat.episode,
        )?
        .unwrap_or_default())
}

fn target_is_stale(store: &crate::Store, heartbeat: &Heartbeat<'_>) -> Result<bool, StoreError> {
    let Some(file) = store.visible_file(heartbeat.user_id, heartbeat.ledger_id)? else {
        return Ok(true);
    };
    if file.media_id != heartbeat.media_id
        || !store.file_covers_unit(
            heartbeat.ledger_id,
            heartbeat.media_id,
            domain::UnitKey::new(heartbeat.season, heartbeat.episode),
        )?
    {
        return Ok(true);
    }
    match store.file_owner_library(heartbeat.ledger_id)? {
        Some(owner) => Ok(!store.library_accepts_work(&owner.id)?),
        None => Ok(false),
    }
}

struct HeartbeatDecision {
    binding: Option<store::PlaySessionBinding>,
    admin_ended: bool,
}

fn classify_heartbeat(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    device: &str,
) -> Result<HeartbeatDecision, StoreError> {
    let event = heartbeat.body["event"].as_str().unwrap_or("progress");
    let previous = store.get_session(heartbeat.user_id, device)?;
    let session_id = heartbeat.body["play_session_id"]
        .as_str()
        .map(str::to_string);
    let binding = match &session_id {
        Some(id) => store.play_session_binding(heartbeat.user_id, device, id)?,
        None => None,
    };
    let session_ended = previous.as_ref().is_some_and(|session| session.admin_ended);
    let admin_ended = session_ended
        || (event != "start"
            && store.device_was_admin_ended(heartbeat.user_id, device, heartbeat.now, 10)?);
    let identity = crate::playback_events::PlaybackEventIdentity {
        play_session_id: session_id,
        event_sequence: heartbeat.body["event_sequence"].as_i64(),
    };
    let kind = match event {
        "start" => crate::playback_events::PlaybackEventKind::Start,
        "stop" => crate::playback_events::PlaybackEventKind::Stop,
        _ => crate::playback_events::PlaybackEventKind::Progress,
    };
    let decision = crate::playback_events::classify_event_with_target(
        previous.as_ref(),
        binding.as_ref(),
        &identity,
        heartbeat.ledger_id,
        Some(domain::UnitKey::new(heartbeat.season, heartbeat.episode)),
        kind,
    );
    Ok(HeartbeatDecision {
        binding: if decision == crate::playback_events::EventDecision::Ignore {
            None
        } else {
            binding
        },
        admin_ended,
    })
}

fn bound_event(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    device: &str,
    binding: store::PlaySessionBinding,
) -> Result<Option<store::playback::BoundPlaybackEvent>, StoreError> {
    let (Some(revision), Some(source_version)) = (binding.file_revision, binding.source_version)
    else {
        return Ok(None);
    };
    let event = heartbeat.body["event"].as_str().unwrap_or("progress");
    let item = if heartbeat.season == UNIT_WHOLE {
        domain::TypedItemId::Work(heartbeat.media_id)
    } else {
        domain::TypedItemId::Unit {
            media: heartbeat.media_id,
            key: domain::UnitKey::new(heartbeat.season, heartbeat.episode),
        }
    };
    // Resume values are valid only for the source observed by this request.
    let resolved =
        store.resolve_playback(heartbeat.user_id, &item, Some(&heartbeat.source), None)?;
    let same_source = resolved.target.file.file_id == heartbeat.source.file_id
        && resolved.target.file.version.revision == heartbeat.source.revision;
    let position_ms = heartbeat.body["position_ms"]
        .as_i64()
        .or_else(|| (event == "start" && same_source).then_some(resolved.start_ms))
        .unwrap_or(0)
        .max(0);
    let event_type = match event {
        "start" => store::playback::PlaybackEventType::Playing,
        "stop" => store::playback::PlaybackEventType::Stopped,
        _ => store::playback::PlaybackEventType::Progress,
    };
    Ok(Some(store::playback::BoundPlaybackEvent {
        user_id: heartbeat.user_id,
        device_id: device.into(),
        session_id: heartbeat.body["play_session_id"]
            .as_str()
            .unwrap_or_default()
            .into(),
        generation: binding.generation,
        event_sequence: heartbeat.body["event_sequence"].as_i64(),
        item,
        source_id: domain::PhysicalSourceId {
            file_id: binding.ledger_id,
            revision,
        },
        source_version,
        position_ms,
        duration_ms: heartbeat
            .duration_ms
            .or(same_source.then_some(resolved.duration_ms).flatten()),
        audio_track: heartbeat.body["audio_track"].as_str().map(str::to_string),
        subtitle_track: heartbeat.body["subtitle_track"]
            .as_str()
            .map(str::to_string),
        paused: heartbeat.body["paused"].as_bool().unwrap_or(false),
        rate_bps: 0,
        client: heartbeat.body["client"].as_str().map(str::to_string),
        device_name: heartbeat.body["device_name"].as_str().map(str::to_string),
        client_version: heartbeat.body["client_version"]
            .as_str()
            .map(str::to_string),
        play_method: None,
        event_type,
    }))
}

fn persist_heartbeat(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    device: &str,
) -> Result<(UnitState, bool), StoreError> {
    if target_is_stale(store, heartbeat)? {
        tracing::warn!(file_id = %heartbeat.ledger_id, media_id = %heartbeat.media_id,
            "Ignoring stale Playback heartbeat: target or coverage changed");
        return Ok((current_unit(store, heartbeat)?, true));
    }
    let decision = classify_heartbeat(store, heartbeat, device)?;
    let Some(binding) = decision.binding else {
        return Ok((current_unit(store, heartbeat)?, decision.admin_ended));
    };
    if let Some(event) = bound_event(store, heartbeat, device, binding)? {
        match store.commit_playback_event(&event, heartbeat.now_ms)? {
            store::playback::EventOutcome::Applied => {
                tracing::debug!(session_id = %event.session_id, "Playback heartbeat committed");
            }
            store::playback::EventOutcome::IgnoredStale => {
                tracing::warn!(session_id = %event.session_id, "Stale Playback heartbeat ignored");
            }
        }
    }
    Ok((current_unit(store, heartbeat)?, decision.admin_ended))
}

fn apply_heartbeat(store: &crate::Store, heartbeat: &Heartbeat<'_>, device: &str) -> Response {
    match persist_heartbeat(store, heartbeat, device) {
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
        let library = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|library| library.kind == domain::MediaKind::Movie)
            .unwrap();
        let provisional = domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "provisional".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let canonical = domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "canonical".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("123".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&provisional).unwrap();
        store.insert_media(&canonical).unwrap();
        let file_id = domain::LedgerId::new();
        let path = library.root_paths[0]
            .join("movie.mkv")
            .display()
            .to_string();
        let spec = store::test_support::FixtureFile {
            file_id: Some(file_id),
            media_id: provisional.id,
            path: path.clone(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
            transfer_mode: None,
            source_path: None,
        };
        store::test_support::publish_fixture_file(&store, &spec).unwrap();
        store.rebind_scanned_ledger(file_id, canonical.id).unwrap();
        let user_id = domain::UserId::new();
        let body = serde_json::json!({"event":"start", "position_ms":500, "device_id":"race"});
        let file = store
            .published_files(&[file_id])
            .unwrap()
            .remove(&file_id)
            .expect("rebound fixture file is published");
        let heartbeat = Heartbeat {
            body: &body,
            user_id,
            media_id: provisional.id,
            ledger_id: file_id,
            season: UNIT_WHOLE,
            episode: UNIT_WHOLE,
            duration_ms: Some(1000),
            now: 10,
            now_ms: 10_000,
            source: domain::PhysicalSourceId {
                file_id: file.file_id,
                revision: file.version.revision,
            },
        };

        let (_, ended) = persist_heartbeat(&store, &heartbeat, "race").unwrap();

        assert!(ended);
        assert!(
            store
                .unit_state(user_id, provisional.id, UNIT_WHOLE, UNIT_WHOLE)
                .unwrap()
                .is_none()
        );
        assert!(store.get_session(user_id, "race").unwrap().is_none());
    }
}
