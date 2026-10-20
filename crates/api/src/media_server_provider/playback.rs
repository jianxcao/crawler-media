use domain::{Media, UserId};
use media_server::provider::{PlaybackClientInfo, PlaybackEvent};

use crate::store::{SessionRow, Store, StoreError, UNIT_WHOLE, UnitState};

use super::ApiServerProvider;

pub(super) struct PlaybackTarget {
    pub(super) media: Media,
    pub(super) ledger_id: String,
    pub(crate) season: i32,
    pub(crate) episode: i32,
    pub(crate) file: domain::LibraryFile,
    duration_ms: Option<i64>,
}

pub(super) fn resolve_target(
    provider: &ApiServerProvider,
    user_id: UserId,
    id: &str,
) -> Result<Option<PlaybackTarget>, String> {
    let entry = {
        let store = provider.state.store.lock();
        super::items::resolve_entry(&store, Some(user_id), id)?
    };
    let Some(entry) = entry else {
        return Ok(None);
    };
    let (season, episode) = (entry.unit.0, entry.unit.1);
    let duration_ms = {
        let store = provider.state.store.lock();
        crate::http::playback::effective_watch_state(
            &store,
            user_id,
            entry.media.id,
            season,
            episode,
        )
        .map_err(|error| error.to_string())?
        .and_then(|unit| unit.duration_ms)
    };
    Ok(Some(PlaybackTarget {
        media: entry.media,
        ledger_id: entry.file.file_id.to_string(),
        season,
        episode,
        file: entry.file,
        duration_ms,
    }))
}

fn persist_position(
    provider: &ApiServerProvider,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    increment_play_count: bool,
    now: i64,
) -> Result<(), String> {
    provider
        .state
        .store
        .lock()
        .playback_write(|store| {
            persist_event_position(
                store,
                user_id,
                target,
                position_ms.max(0),
                increment_play_count,
                now,
            )
        })
        .map_err(|error| error.to_string())
}

pub(super) fn update_progress(
    provider: &ApiServerProvider,
    user_id: UserId,
    id: &str,
    position_ms: i64,
) -> Result<(), String> {
    let target = resolve_target(provider, user_id, id)?
        .ok_or_else(|| format!("Jellyfin playback item {id} is missing or not visible"))?;
    let now = provider.state.playback_clock.now_ms() / 1000;
    persist_position(provider, user_id, &target, position_ms, false, now)
}

pub(super) fn report_event(
    provider: &ApiServerProvider,
    user_id: UserId,
    id: &str,
    position_ms: i64,
    paused: bool,
    client: PlaybackClientInfo,
    event: PlaybackEvent,
    identity: media_server::provider::PlaybackEventIdentity,
) -> Result<(), String> {
    let target = resolve_target(provider, user_id, id)?
        .ok_or_else(|| format!("Jellyfin playback item {id} is missing or not visible"))?;
    let target_ledger_id = target.ledger_id.parse::<domain::LedgerId>().map_err(|e| e.to_string())?;
    let now_ms = provider.state.playback_clock.now_ms();
    let now = now_ms / 1000;
    let position_ms = position_ms.max(0);
    let store = provider.state.store.lock();
    if store
        .is_device_revoked(user_id, &client.device_id)
        .map_err(|error| error.to_string())?
    {
        return Err("device revoked".into());
    }

    // Classify event using shared logic
    let prior = store.get_session(user_id, &client.device_id).map_err(|e| e.to_string())?;
    let binding = if let Some(ref sid) = identity.play_session_id {
        store.play_session_binding(user_id, &client.device_id, sid).map_err(|e| e.to_string())?
    } else {
        None
    };

    let shared_identity = crate::playback_events::PlaybackEventIdentity {
        play_session_id: identity.play_session_id.clone(),
        event_sequence: identity.event_sequence,
    };

    let event_kind = match event {
        PlaybackEvent::Playing => crate::playback_events::PlaybackEventKind::Start,
        PlaybackEvent::Progress => crate::playback_events::PlaybackEventKind::Progress,
        PlaybackEvent::Stopped => crate::playback_events::PlaybackEventKind::Stop,
    };

    let decision = crate::playback_events::classify_event(
        prior.as_ref(),
        binding.as_ref(),
        &shared_identity,
        target_ledger_id,
        event_kind,
    );
    if decision == crate::playback_events::EventDecision::Ignore {
        return Ok(());
    }
    // Legacy clients omit `PlaySessionId`. Keep their historical semantics for
    // classification (the no-id branch above), but persist through a real
    // issued id: reuse the active session for this target, else issue one.
    // Clients that do send an id are never rewritten here, so an unknown id is
    // still rejected by classification.
    let session_id = match identity.play_session_id {
        Some(sid) => Some(sid),
        None => {
            let reused = prior
                .as_ref()
                .filter(|session| same_target(session, &target))
                .and_then(|session| session.play_session_id.clone());
            match reused {
                Some(sid) => Some(sid),
                None => Some(
                    store
                        .issue_play_session(
                            user_id,
                            &client.device_id,
                            target_ledger_id,
                            target.media.id,
                            domain::UnitKey::new(target.season, target.episode),
                            &target.file.version,
                            now,
                        )
                        .map_err(|error| error.to_string())?,
                ),
            }
        }
    };

    store
        .playback_write(|store| {
            match decision {
                crate::playback_events::EventDecision::Ignore => return Ok(()),
                crate::playback_events::EventDecision::Start => {
                    if let Some(sid) = &session_id {
                        store.activate_play_session(user_id, &client.device_id, sid)?;
                    }
                }
                crate::playback_events::EventDecision::Stop => {
                    if let Some(sid) = &session_id {
                        store.close_play_session_id(user_id, &client.device_id, sid)?;
                    }
                }
                crate::playback_events::EventDecision::Update => {}
            }

            persist_event(
                store,
                user_id,
                &target,
                position_ms,
                paused,
                &client,
                event,
                session_id.as_deref(),
                identity.event_sequence,
                now,
                now_ms,
            )
        })
        .map_err(|error| error.to_string())
}
#[allow(clippy::too_many_arguments)]
fn persist_event(
    store: &Store,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    paused: bool,
    client: &PlaybackClientInfo,
    event: PlaybackEvent,
    play_session_id: Option<&str>,
    event_sequence: Option<i64>,
    now: i64,
    now_ms: i64,
) -> Result<(), StoreError> {
    let current = current_session(store, user_id, &client.device_id, target, play_session_id, now)?;
    if current.as_ref().is_some_and(|s| s.admin_ended) {
        return Ok(());
    }
    // 会话已被管理员结束（此时 current 为 None）时，迟到的 Progress/Stopped 不得
    // 把会话复活；只有客户端显式重新起播（Playing）才允许新建会话。与 web 心跳
    // 路径的 `device_was_admin_ended` 守卫保持同一语义。
    if current.is_none() && event != PlaybackEvent::Playing {
        match store.device_was_admin_ended(user_id, &client.device_id, now, 10) {
            Ok(true) => {
                tracing::debug!(
                    %user_id,
                    device_id = %client.device_id,
                    "管理员已结束该设备会话，忽略迟到事件"
                );
                return Ok(());
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, %user_id, device_id = %client.device_id, "查询管理员结束标记失败");
            }
        }
    }
    // 验证目标所属媒体库未处于删除冻结中且台账未被清理，防止已删除文件在心跳中复活
    if let Some(row) = store.get_ledger(&target.ledger_id)? {
        if let Some(owner) = store.library_for_path_strict(std::path::Path::new(&row.path), target.media.kind)? {
            if !store.library_accepts_work(&owner.id)? {
                return Ok(());
            }
        }
    } else {
        return Ok(());
    }

    persist_event_position(
        store,
        user_id,
        target,
        position_ms,
        event == PlaybackEvent::Playing && current.is_none(),
        now,
    )?;
    let watched_ms = accumulated_watch_time(
        current.as_ref(),
        position_ms,
        paused,
        event == PlaybackEvent::Stopped,
        now_ms,
    );
    if event == PlaybackEvent::Stopped {
        return stop_session(
            store,
            client,
            user_id,
            target,
            position_ms,
            watched_ms,
            current.as_ref(),
            now,
            now_ms,
        );
    }
    store.upsert_session(&session_row(
        client,
        user_id,
        target,
        position_ms,
        paused,
        watched_ms,
        current.as_ref(),
        play_session_id,
        event_sequence,
        now,
        now_ms,
    ))
}
fn current_session(
    store: &Store,
    user_id: UserId,
    device_id: &str,
    target: &PlaybackTarget,
    play_session_id: Option<&str>,
    now: i64,
) -> Result<Option<SessionRow>, StoreError> {
    let prior = store.get_session(user_id, device_id)?;
    if prior
        .as_ref()
        .is_some_and(|session| {
            same_target(session, target)
                && match (session.play_session_id.as_deref(), play_session_id) {
                    (Some(prior_id), Some(new_id)) => prior_id == new_id,
                    _ => true,
                }
        })
    {
        return Ok(prior);
    }
    if prior.is_some() {
        store.close_session(user_id, device_id, now)?;
    }
    Ok(None)
}

fn same_target(session: &SessionRow, target: &PlaybackTarget) -> bool {
    session.media_id == target.media.id
        && session.season.unwrap_or(UNIT_WHOLE) == unit_value(target.season).unwrap_or(UNIT_WHOLE)
        && session.episode.unwrap_or(UNIT_WHOLE) == unit_value(target.episode).unwrap_or(UNIT_WHOLE)
}

fn persist_event_position(
    store: &Store,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    increment_play_count: bool,
    now: i64,
) -> Result<(), StoreError> {
    // canonical whole-media coordinate (-1, -1) used directly
    let duration = target.duration_ms.or(store
        .unit_state(user_id, target.media.id, target.season, target.episode)?
        .and_then(|unit| unit.duration_ms));
    store.upsert_unit_for_source(
        user_id,
        target.media.id,
        domain::UnitKey::new(target.season, target.episode),
        domain::PhysicalSourceId { file_id: target.file.file_id, revision: target.file.version.revision },
        position_ms,
        UnitState::playback_completed(position_ms, duration).then_some(true),
        None,
        duration,
        None,
        None,
        increment_play_count,
        now,
    )?;
    Ok(())
}

fn accumulated_watch_time(
    current: Option<&SessionRow>,
    position_ms: i64,
    paused: bool,
    stopped: bool,
    now_ms: i64,
) -> i64 {
    let Some(session) = current else {
        return 0;
    };
    let delta = crate::playback_events::watched_increment(session, position_ms, paused, stopped, now_ms, 1.0);
    session.watched_ms.saturating_add(delta)
}

fn stop_session(
    store: &Store,
    client: &PlaybackClientInfo,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    watched_ms: i64,
    existing: Option<&SessionRow>,
    now: i64,
    now_ms: i64,
) -> Result<(), StoreError> {
    if existing.is_none() {
        return Ok(());
    }
    store.upsert_session(&session_row(
        client,
        user_id,
        target,
        position_ms,
        true,
        watched_ms,
        existing,
        existing.and_then(|s| s.play_session_id.as_deref()),
        existing.and_then(|s| s.last_event_sequence),
        now,
        now_ms,
    ))?;
    store.close_session(user_id, &client.device_id, now)?;
    Ok(())
}

fn unit_value(value: i32) -> Option<i32> {
    (value != UNIT_WHOLE).then_some(value)
}

#[allow(clippy::too_many_arguments)]
fn session_row(
    client: &PlaybackClientInfo,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    paused: bool,
    watched_ms: i64,
    existing: Option<&SessionRow>,
    play_session_id: Option<&str>,
    event_sequence: Option<i64>,
    now: i64,
    now_ms: i64,
) -> SessionRow {
    SessionRow {
        device_id: client.device_id.clone(),
        user_id,
        media_id: target.media.id,
        season: unit_value(target.season),
        episode: unit_value(target.episode),
        client: client
            .client
            .clone()
            .or_else(|| existing.and_then(|session| session.client.clone())),
        device_name: client
            .device_name
            .clone()
            .or_else(|| existing.and_then(|session| session.device_name.clone())),
        client_version: client
            .client_version
            .clone()
            .or_else(|| existing.and_then(|session| session.client_version.clone())),
        play_method: "DirectPlay".to_string(),
        position_ms,
        start_position_ms: existing
            .map(|session| session.start_position_ms)
            .unwrap_or(position_ms),
        duration_ms: target
            .duration_ms
            .or_else(|| existing.and_then(|session| session.duration_ms)),
        paused,
        watched_ms,
        rate_bps: existing.map(|session| session.rate_bps).unwrap_or(0),
        bytes_sent: existing.map(|session| session.bytes_sent).unwrap_or(0),
        connections: existing.map(|session| session.connections).unwrap_or(1),
        admin_ended: existing.map(|session| session.admin_ended).unwrap_or(false),
        started_at: existing.map(|session| session.started_at).unwrap_or(now),
        last_report_at: now,
        ledger_id: Some(target.ledger_id.clone()),
        play_session_id: play_session_id.map(str::to_string).or_else(|| existing.and_then(|s| s.play_session_id.clone())),
        last_report_at_ms: now_ms,
        last_event_sequence: event_sequence.or_else(|| existing.and_then(|s| s.last_event_sequence)),
    }
}
