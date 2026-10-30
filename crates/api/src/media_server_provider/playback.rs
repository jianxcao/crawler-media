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
        let store = provider.bounded_reader()?;
        super::items::resolve_entry(&store, Some(user_id), id)?
    };
    let Some(entry) = entry else {
        return Ok(None);
    };
    let (season, episode) = (entry.unit.0, entry.unit.1);
    // Duration belongs to the physical source: prefer the probed duration of this file and
    // only fall back to a stored user value recorded against the same source.
    let duration_ms = match entry.file.duration_ms.filter(|d| *d > 0) {
        Some(duration) => Some(duration),
        None => {
            let item = if season == crate::store::UNIT_WHOLE {
                domain::TypedItemId::Work(entry.media.id)
            } else {
                domain::TypedItemId::Unit {
                    media: entry.media.id,
                    key: domain::UnitKey::new(season, episode),
                }
            };
            let source = domain::PhysicalSourceId {
                file_id: entry.file.file_id,
                revision: entry.file.version.revision,
            };
            let store = provider.bounded_reader()?;
            let resolved = store
                .resolve_playback(user_id, &item, Some(&source), None)
                .map_err(|error| error.to_string())?;
            (resolved.target.file.file_id == source.file_id
                && resolved.target.file.version.revision == source.revision)
                .then_some(resolved.duration_ms)
                .flatten()
        }
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
    let now_ms = provider.state.playback_clock.now_ms();
    let store = provider.bounded_store()?;
    if !provider.state.instance_writable() {
        return Err("instance is read-only or under maintenance".into());
    }
    if store
        .is_device_revoked(user_id, &client.device_id)
        .map_err(|error| error.to_string())?
    {
        return Err("device revoked".into());
    }
    let request = ProtocolEvent {
        user_id,
        target: &target,
        client: &client,
        event,
        identity: &identity,
        now: now_ms / 1000,
    };
    let Some((sid, binding)) = request.bound_session(&store)? else {
        return Ok(());
    };
    let Some(bound) = request.bound_event(sid, binding, position_ms, paused) else {
        return Ok(());
    };
    match store.commit_playback_event(&bound, now_ms) {
        Ok(store::playback::EventOutcome::Applied) => {
            tracing::debug!(session_id=%bound.session_id,"Jellyfin Playback event committed");
        }
        Ok(store::playback::EventOutcome::IgnoredStale) => {
            tracing::warn!(session_id=%bound.session_id,"Stale Jellyfin Playback event ignored");
        }
        Err(error) => {
            tracing::error!(session_id=%bound.session_id,%error,"Jellyfin Playback event failed");
            return Err(error.to_string());
        }
    }
    Ok(())
}

struct ProtocolEvent<'a> {
    user_id: UserId,
    target: &'a PlaybackTarget,
    client: &'a PlaybackClientInfo,
    event: PlaybackEvent,
    identity: &'a media_server::provider::PlaybackEventIdentity,
    now: i64,
}

impl ProtocolEvent<'_> {
    fn bound_session(
        &self,
        store: &Store,
    ) -> Result<Option<(String, store::PlaySessionBinding)>, String> {
        let prior = store
            .get_session(self.user_id, &self.client.device_id)
            .map_err(|error| error.to_string())?;
        let binding = match &self.identity.play_session_id {
            Some(id) => store
                .play_session_binding(self.user_id, &self.client.device_id, id)
                .map_err(|error| error.to_string())?,
            None => None,
        };
        let kind = match self.event {
            PlaybackEvent::Playing => crate::playback_events::PlaybackEventKind::Start,
            PlaybackEvent::Progress => crate::playback_events::PlaybackEventKind::Progress,
            PlaybackEvent::Stopped => crate::playback_events::PlaybackEventKind::Stop,
        };
        let decision = if self.identity.play_session_id.is_none() {
            classify_protocol_event(prior.as_ref(), self.target, self.event)
        } else {
            crate::playback_events::classify_event_with_target(
                prior.as_ref(),
                binding.as_ref(),
                &crate::playback_events::PlaybackEventIdentity {
                    play_session_id: self.identity.play_session_id.clone(),
                    event_sequence: self.identity.event_sequence,
                },
                self.target.file.file_id,
                Some(domain::UnitKey::new(
                    self.target.season,
                    self.target.episode,
                )),
                kind,
            )
        };
        if decision == crate::playback_events::EventDecision::Ignore {
            return Ok(None);
        }
        let id = if let Some(id) = &self.identity.play_session_id {
            id.clone()
        } else if let Some(id) = self.reusable_session(store, prior.as_ref())? {
            id
        } else if self.event == PlaybackEvent::Playing {
            store
                .issue_play_session(
                    self.user_id,
                    &self.client.device_id,
                    self.target.file.file_id,
                    self.target.media.id,
                    domain::UnitKey::new(self.target.season, self.target.episode),
                    &self.target.file.version,
                    self.now,
                )
                .map_err(|error| error.to_string())?
        } else {
            return Ok(None);
        };
        // All protocol events commit a real binding, including clients that omit PlaySessionId.
        let binding = store
            .play_session_binding(self.user_id, &self.client.device_id, &id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "issued Jellyfin Playback binding disappeared".to_string())?;
        Ok(Some((id, binding)))
    }

    fn reusable_session(
        &self,
        store: &Store,
        prior: Option<&SessionRow>,
    ) -> Result<Option<String>, String> {
        let Some(prior) = prior.filter(|session| same_target(session, self.target)) else {
            return Ok(None);
        };
        let Some(id) = &prior.play_session_id else {
            return Ok(None);
        };
        let binding = store
            .play_session_binding(self.user_id, &self.client.device_id, id)
            .map_err(|error| error.to_string())?;
        Ok(binding
            .filter(|binding| {
                binding.ledger_id == self.target.file.file_id
                    && binding.file_revision == Some(self.target.file.version.revision)
                    && binding.source_version.as_deref()
                        == Some(self.target.file.version.source_version.as_str())
                    && binding.state != store::PlaySessionIdState::Closed
            })
            .map(|_| id.clone()))
    }

    fn bound_event(
        &self,
        id: String,
        binding: store::PlaySessionBinding,
        position_ms: i64,
        paused: bool,
    ) -> Option<store::playback::BoundPlaybackEvent> {
        let (Some(revision), Some(source_version)) =
            (binding.file_revision, binding.source_version)
        else {
            return None;
        };
        let item = if self.target.season == UNIT_WHOLE {
            domain::TypedItemId::Work(self.target.media.id)
        } else {
            domain::TypedItemId::Unit {
                media: self.target.media.id,
                key: domain::UnitKey::new(self.target.season, self.target.episode),
            }
        };
        let event_type = match self.event {
            PlaybackEvent::Playing => store::playback::PlaybackEventType::Playing,
            PlaybackEvent::Progress => store::playback::PlaybackEventType::Progress,
            PlaybackEvent::Stopped => store::playback::PlaybackEventType::Stopped,
        };
        Some(store::playback::BoundPlaybackEvent {
            user_id: self.user_id,
            device_id: self.client.device_id.clone(),
            session_id: id,
            generation: binding.generation,
            event_sequence: self.identity.event_sequence,
            item,
            source_id: domain::PhysicalSourceId {
                file_id: binding.ledger_id,
                revision,
            },
            source_version,
            position_ms: position_ms.max(0),
            duration_ms: self.target.duration_ms,
            audio_track: None,
            subtitle_track: None,
            paused,
            rate_bps: 0,
            client: self.client.client.clone(),
            device_name: self.client.device_name.clone(),
            client_version: self.client.client_version.clone(),
            play_method: Some("DirectPlay".into()),
            event_type,
        })
    }
}

fn same_target(session: &SessionRow, target: &PlaybackTarget) -> bool {
    session.media_id == target.media.id
        && session.ledger_id.as_deref() == Some(target.ledger_id.as_str())
        && session.season.unwrap_or(UNIT_WHOLE) == target.season
        && session.episode.unwrap_or(UNIT_WHOLE) == target.episode
}

fn classify_protocol_event(
    previous: Option<&SessionRow>,
    target: &PlaybackTarget,
    event: PlaybackEvent,
) -> crate::playback_events::EventDecision {
    use crate::playback_events::EventDecision;
    match event {
        PlaybackEvent::Playing => EventDecision::Start,
        PlaybackEvent::Progress if previous.is_some_and(|session| same_target(session, target)) => {
            EventDecision::Update
        }
        PlaybackEvent::Stopped if previous.is_some_and(|session| same_target(session, target)) => {
            EventDecision::Stop
        }
        _ => EventDecision::Ignore,
    }
}
