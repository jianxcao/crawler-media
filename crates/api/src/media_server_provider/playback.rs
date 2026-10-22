use domain::{Media, UserId};
use media_server::provider::{PlaybackClientInfo, PlaybackEvent};

use crate::store::{SessionRow, Store, UNIT_WHOLE};

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
    let target_ledger_id = target
        .ledger_id
        .parse::<domain::LedgerId>()
        .map_err(|e| e.to_string())?;
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
    let prior = store
        .get_session(user_id, &client.device_id)
        .map_err(|e| e.to_string())?;
    let binding = if let Some(ref sid) = identity.play_session_id {
        store
            .play_session_binding(user_id, &client.device_id, sid)
            .map_err(|e| e.to_string())?
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

    let decision = crate::playback_events::classify_event_with_target(
        prior.as_ref(),
        binding.as_ref(),
        &shared_identity,
        target_ledger_id,
        Some(domain::UnitKey::new(target.season, target.episode)),
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
    let reused = prior
        .as_ref()
        .filter(|session| same_target(session, &target))
        .and_then(|session| session.play_session_id.clone());
    let session = effective_session_id(
        &store,
        user_id,
        &client,
        &target,
        target_ledger_id,
        identity.play_session_id.clone(),
        reused,
        binding.as_ref(),
        event,
        now,
    )?;

    // Commit through canonical bound playback event for unit fact synchronization
    let item = if target.season == UNIT_WHOLE {
        domain::TypedItemId::Work(target.media.id)
    } else {
        domain::TypedItemId::Unit {
            media: target.media.id,
            key: domain::UnitKey::new(target.season, target.episode),
        }
    };
    let event_type = match event {
        PlaybackEvent::Playing => store::playback::PlaybackEventType::Playing,
        PlaybackEvent::Stopped => store::playback::PlaybackEventType::Stopped,
        PlaybackEvent::Progress => store::playback::PlaybackEventType::Progress,
    };
    let sid = session.id;
    // 无 id 的旧客户端：本次事件用的 session 由上面刚签发/复用，代际只有绑定里才有。
    // 声明正确的 generation，事件才属于这个已签发代际，而不是被当成过期事件丢弃。
    let generation = session.generation;
    let bound_source = if let Some(ref b) = binding {
        domain::PhysicalSourceId {
            file_id: b.ledger_id,
            revision: b.file_revision.unwrap_or(target.file.version.revision),
        }
    } else {
        domain::PhysicalSourceId {
            file_id: target.file.file_id,
            revision: target.file.version.revision,
        }
    };
    let bound_event = store::playback::BoundPlaybackEvent {
        user_id,
        device_id: client.device_id.clone(),
        session_id: sid.clone(),
        generation,
        event_sequence: identity.event_sequence,
        item,
        source_id: bound_source,
        source_version: binding
            .as_ref()
            .and_then(|b| b.source_version.clone())
            .unwrap_or_else(|| target.file.version.source_version.clone()),
        position_ms,
        duration_ms: target.duration_ms,
        audio_track: None,
        subtitle_track: None,
        paused,
        rate_bps: 0,
        client: client.client.clone(),
        device_name: client.device_name.clone(),
        client_version: client.client_version.clone(),
        play_method: Some("DirectPlay".to_string()),
        event_type,
    };
    match store.commit_playback_event(&bound_event, now_ms) {
        Ok(store::playback::EventOutcome::Applied) => {
            tracing::debug!(session_id = %sid, "Jellyfin commit_playback_event applied");
        }
        Ok(store::playback::EventOutcome::IgnoredStale) => {
            tracing::warn!(session_id = %sid, "Jellyfin commit_playback_event ignored as stale");
        }
        Err(e) => {
            tracing::error!(session_id = %sid, error = %e, "Jellyfin commit_playback_event failed");
            return Err(e.to_string());
        }
    }

    Ok(())
}

/// 本次事件真正落库的 play session 及其代际。
///
/// 代际必须来自签发/复用得到的绑定行：canonical 的 `binding_matches`
/// （`crates/store/src/playback/event.rs:225`）逐字比对 `generation`，
/// 用默认值声明会让本次事件被判为过期事件而零写入。
struct EffectivePlaySession {
    id: String,
    generation: i64,
}

/// 决定本次事件真正落库的 play session：显式 id 原样透传，不带 id 的旧客户端
/// 复用活跃 session，只有 Playing 才新开一个；无 id 可复用也不签发时返回空 id
/// （canonical 的 unbound 分支只写单元事实，不签发会话、不写播放日志）。
#[allow(clippy::too_many_arguments)]
fn effective_session_id(
    store: &Store,
    user_id: UserId,
    client: &PlaybackClientInfo,
    target: &PlaybackTarget,
    target_ledger_id: domain::LedgerId,
    explicit: Option<String>,
    reused: Option<String>,
    binding: Option<&crate::store::PlaySessionBinding>,
    event: PlaybackEvent,
    now: i64,
) -> Result<EffectivePlaySession, String> {
    if let Some(sid) = explicit {
        return Ok(EffectivePlaySession {
            generation: binding.map(|b| b.generation).unwrap_or(0),
            id: sid,
        });
    }
    if let Some(sid) = reused {
        return Ok(EffectivePlaySession {
            generation: bound_generation(store, user_id, &client.device_id, &sid)?,
            id: sid,
        });
    }
    if event != PlaybackEvent::Playing {
        tracing::warn!(
            user_id = %user_id,
            device_id = %client.device_id,
            "Progress/Stop without active or explicit play session: ignoring fabricated session issuance"
        );
        return Ok(EffectivePlaySession {
            id: String::new(),
            generation: 0,
        });
    }
    let sid = store
        .issue_play_session(
            user_id,
            &client.device_id,
            target_ledger_id,
            target.media.id,
            domain::UnitKey::new(target.season, target.episode),
            &target.file.version,
            now,
        )
        .map_err(|error| error.to_string())?;
    Ok(EffectivePlaySession {
        generation: bound_generation(store, user_id, &client.device_id, &sid)?,
        id: sid,
    })
}

/// 绑定行声明的代际。没有绑定行时返回 0：canonical 的绑定比对会把这个事件判为
/// 过期（`IgnoredStale`，零写入），而不是无绑定地接受它。
fn bound_generation(
    store: &Store,
    user_id: UserId,
    device_id: &str,
    play_session_id: &str,
) -> Result<i64, String> {
    Ok(store
        .play_session_binding(user_id, device_id, play_session_id)
        .map_err(|error| error.to_string())?
        .map(|binding| binding.generation)
        .unwrap_or(0))
}

fn same_target(session: &SessionRow, target: &PlaybackTarget) -> bool {
    session.media_id == target.media.id
        && session.season.unwrap_or(UNIT_WHOLE) == unit_value(target.season).unwrap_or(UNIT_WHOLE)
        && session.episode.unwrap_or(UNIT_WHOLE) == unit_value(target.episode).unwrap_or(UNIT_WHOLE)
}

fn unit_value(value: i32) -> Option<i32> {
    (value != UNIT_WHOLE).then_some(value)
}
