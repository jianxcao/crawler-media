//! Live playback session transitions and play-log archiving for bound events.
use rusqlite::{OptionalExtension, params};

use super::{
    BoundPlaybackEvent, IssuedBinding, PlayLogRow, PlaybackEventType, StoreError, UnitState,
    playback_unit,
};

/// A new `PlaySessionId` on the same (user, device) supersedes the live row of the old one:
/// archive that row under its *own* binding (one play log) and close its id, so the new
/// session starts with fresh `started_at`/`watched_ms` instead of inheriting them.
pub(super) fn archive_superseded_session(
    store: &crate::LibraryConnection<'_>,
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    now_ms: i64,
) -> Result<(), StoreError> {
    let Some(previous) = store.get_session(event.user_id, &event.device_id)? else {
        return Ok(());
    };
    let Some(old_id) = previous.play_session_id.clone() else {
        return Ok(());
    };
    if old_id == event.session_id {
        return Ok(());
    }
    // Only a newer issued generation supersedes; a late event of an older session never
    // archives the session that replaced it.
    let old_generation: Option<i64> = tx.query_row(
        "SELECT generation FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
        params![event.user_id.to_string(), event.device_id],
        |r| r.get(0),
    )?;
    if old_generation.is_some_and(|g| g >= event.generation) {
        return Ok(());
    }
    let completed = UnitState::playback_completed(previous.position_ms, previous.duration_ms);
    let (user, device) = (event.user_id.to_string(), event.device_id.as_str());
    tx.execute(
        "INSERT INTO playback_logs
            (id, user_id, media_id, season, episode, client, device_name, play_method,
             started_at, ended_at, watched_ms, start_position_ms, end_position_ms,
             duration_ms, completed, device_id, admin_ended, play_session_id, generation,
             file_id, file_revision, source_version, timeline_mode)
         SELECT ?3, user_id, media_id, season, episode, client, device_name, play_method,
                started_at, ?4, watched_ms, start_position_ms, position_ms, duration_ms, ?5,
                device_id, admin_ended, play_session_id, generation, file_id, file_revision,
                source_version, COALESCE(timeline_mode, 'full_file')
         FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2
         ON CONFLICT(user_id, device_id, play_session_id, generation) DO UPDATE SET
            ended_at = excluded.ended_at,
            watched_ms = playback_logs.watched_ms + excluded.watched_ms,
            end_position_ms = excluded.end_position_ms,
            duration_ms = excluded.duration_ms, completed = excluded.completed,
            admin_ended = MAX(playback_logs.admin_ended, excluded.admin_ended)",
        params![
            user,
            device,
            uuid::Uuid::new_v4().to_string(),
            now_ms / 1000,
            completed as i64
        ],
    )?;
    tx.execute(
        "DELETE FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
        params![user, device],
    )?;
    tx.execute(
        "UPDATE playback_session_ids SET state = 'closed'
         WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
        params![user, device, old_id],
    )?;
    Ok(())
}

pub(super) fn close_live_session(
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    resolved_duration: Option<i64>,
    now_ms: i64,
) -> Result<(), StoreError> {
    let session: Option<crate::playback::SessionRow> = tx
        .query_row(
            "SELECT device_id, user_id, media_id, season, episode, client, device_name,
                    client_version, play_method, position_ms, start_position_ms,
                    duration_ms, paused, watched_ms, rate_bps, bytes_sent, connections,
                    admin_ended, started_at, last_report_at, file_id, play_session_id,
                    last_report_at_ms, last_event_sequence
             FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
            params![event.user_id.to_string(), event.device_id],
            crate::playback::sessions::map_session,
        )
        .optional()?;

    let Some(session) = session else {
        return finish_archived_session(tx, event, binding, resolved_duration, now_ms);
    };

    tx.execute(
        "DELETE FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
        params![event.user_id.to_string(), event.device_id],
    )?;

    let now = now_ms / 1000;
    let duration_ms = resolved_duration
        .filter(|duration| *duration > 0)
        .or_else(|| session.duration_ms.filter(|duration| *duration > 0));
    let completed = UnitState::playback_completed(session.position_ms, duration_ms);
    let log = PlayLogRow {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: session.user_id,
        media_id: session.media_id,
        season: session.season,
        episode: session.episode,
        device_id: session.device_id,
        client: session.client,
        device_name: session.device_name,
        play_method: session.play_method,
        started_at: session.started_at,
        ended_at: now,
        watched_ms: session.watched_ms,
        start_position_ms: session.start_position_ms,
        end_position_ms: session.position_ms,
        duration_ms,
        completed,
    };

    tx.execute(
        "INSERT INTO playback_logs
            (id, user_id, media_id, season, episode, client, device_name, play_method,
             started_at, ended_at, watched_ms, start_position_ms, end_position_ms,
             duration_ms, completed, device_id, admin_ended, play_session_id, generation,
             file_id, file_revision, source_version, timeline_mode)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18, ?19, ?20, ?21, ?22, ?23)
         ON CONFLICT(user_id, device_id, play_session_id, generation) DO UPDATE SET
            ended_at = excluded.ended_at,
            watched_ms = playback_logs.watched_ms + excluded.watched_ms,
            end_position_ms = excluded.end_position_ms,
            duration_ms = excluded.duration_ms, completed = excluded.completed,
            admin_ended = MAX(playback_logs.admin_ended, excluded.admin_ended)",
        params![
            log.id,
            log.user_id.to_string(),
            log.media_id.to_string(),
            log.season,
            log.episode,
            log.client,
            log.device_name,
            log.play_method,
            log.started_at,
            log.ended_at,
            log.watched_ms,
            log.start_position_ms,
            log.end_position_ms,
            log.duration_ms,
            log.completed as i64,
            log.device_id,
            session.admin_ended as i64,
            event.session_id,
            binding.generation,
            binding.file_id,
            binding.file_revision,
            binding.source_version,
            "full_file",
        ],
    )?;
    Ok(())
}

/// A Stop after timeout still supplies the final position for the archived session.
/// No live interval exists to contribute watch time.
fn finish_archived_session(
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    resolved_duration: Option<i64>,
    now_ms: i64,
) -> Result<(), StoreError> {
    let previous: Option<(i64, Option<i64>)> = tx
        .query_row(
            "SELECT end_position_ms, duration_ms FROM playback_logs
         WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3 AND generation = ?4",
            params![
                event.user_id.to_string(),
                event.device_id,
                event.session_id,
                binding.generation
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((position, duration)) = previous else {
        return Ok(());
    };
    let position = event.position_ms.unwrap_or(position);
    let duration = resolved_duration
        .filter(|duration| *duration > 0)
        .or_else(|| event.duration_ms.filter(|duration| *duration > 0))
        .or(duration);
    tx.execute(
        "UPDATE playback_logs SET ended_at = ?5, end_position_ms = ?6,
             duration_ms = ?7, completed = ?8
         WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3 AND generation = ?4",
        params![
            event.user_id.to_string(),
            event.device_id,
            event.session_id,
            binding.generation,
            now_ms / 1000,
            position,
            duration,
            UnitState::playback_completed(position, duration)
        ],
    )?;
    Ok(())
}

/// A `Stop` closes exactly the session that reported it and drops its live session row; a
/// repeated `Stop` never reaches here because the binding is already `closed`.
pub(super) fn advance_session_state(
    store: &crate::LibraryConnection<'_>,
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    resolved_duration: Option<i64>,
    now_ms: i64,
) -> Result<(), StoreError> {
    archive_superseded_session(store, tx, event, now_ms)?;
    let previous = store.get_session(event.user_id, &event.device_id)?;
    if event.event_type == PlaybackEventType::Stopped {
        // A `Stop` carries the final position and may be the first event to report duration:
        // record both on the live row before archiving so the play log agrees with unit state.
        if let Some(session) = previous.as_ref() {
            let watched_ms =
                session
                    .watched_ms
                    .saturating_add(watch_increment(Some(session), event, now_ms));
            let final_duration = resolved_duration
                .filter(|duration| *duration > 0)
                .or_else(|| session.duration_ms.filter(|duration| *duration > 0));
            tx.execute(
                "UPDATE playback_sessions
                 SET position_ms = ?3, duration_ms = ?4, paused = 1,
                     watched_ms = ?5, last_report_at_ms = ?6
                 WHERE user_id = ?1 AND device_id = ?2",
                params![
                    event.user_id.to_string(),
                    event.device_id,
                    event.position_ms.unwrap_or(session.position_ms),
                    final_duration,
                    watched_ms,
                    now_ms
                ],
            )?;
        }
    }
    if event.event_type == PlaybackEventType::Stopped
        || previous.as_ref().is_some_and(|s| s.admin_ended)
    {
        tx.execute(
            "UPDATE playback_session_ids SET state = 'closed'
             WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
            params![event.user_id.to_string(), event.device_id, event.session_id],
        )?;
        close_live_session(tx, event, binding, resolved_duration, now_ms)?;
        return Ok(());
    }
    upsert_live_session(store, tx, event, binding, resolved_duration, now_ms)
}

/// Create or advance the live `playback_sessions` row for this session.
fn upsert_live_session(
    store: &crate::LibraryConnection<'_>,
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    resolved_duration: Option<i64>,
    now_ms: i64,
) -> Result<(), StoreError> {
    let previous = store.get_session(event.user_id, &event.device_id)?;
    let started_at = previous
        .as_ref()
        .map(|session| session.started_at)
        .unwrap_or(now_ms / 1000);
    let position_ms = event
        .position_ms
        .or_else(|| previous.as_ref().map(|s| s.position_ms))
        .unwrap_or(0);
    let start_position_ms = previous
        .as_ref()
        .map(|session| session.start_position_ms)
        .unwrap_or(position_ms);
    let watched_ms = previous
        .as_ref()
        .map(|session| session.watched_ms)
        .unwrap_or(0)
        + watch_increment(previous.as_ref(), event, now_ms);
    let play_method = event
        .play_method
        .clone()
        .or_else(|| previous.as_ref().map(|session| session.play_method.clone()))
        .unwrap_or_else(|| "local".to_string());
    let duration_ms = resolved_duration
        .filter(|duration| *duration > 0)
        .or_else(|| {
            previous
                .as_ref()
                .and_then(|session| session.duration_ms.filter(|duration| *duration > 0))
        });
    let (media_id, unit) = playback_unit(&event.item);
    tx.execute(
        "INSERT INTO playback_sessions
            (user_id, device_id, media_id, season, episode, client, device_name, client_version,
             play_method, position_ms, start_position_ms, duration_ms, paused, watched_ms, rate_bps,
             started_at, last_report_at, file_id, file_revision, source_version, library_id,
             config_revision, timeline_mode, play_session_id, generation, last_report_at_ms,
             last_event_sequence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)
         ON CONFLICT(user_id, device_id) DO UPDATE SET
            media_id = excluded.media_id, season = excluded.season, episode = excluded.episode,
            client = COALESCE(excluded.client, playback_sessions.client),
            device_name = COALESCE(excluded.device_name, playback_sessions.device_name),
            client_version = COALESCE(excluded.client_version, playback_sessions.client_version),
            play_method = excluded.play_method,
            position_ms = excluded.position_ms, duration_ms = excluded.duration_ms,
            paused = excluded.paused, watched_ms = excluded.watched_ms,
            rate_bps = excluded.rate_bps,
            last_report_at = excluded.last_report_at,
            file_id = excluded.file_id, file_revision = excluded.file_revision,
            source_version = excluded.source_version, library_id = excluded.library_id,
            config_revision = excluded.config_revision,
            timeline_mode = excluded.timeline_mode,
            play_session_id = excluded.play_session_id, generation = excluded.generation,
            last_report_at_ms = excluded.last_report_at_ms,
            last_event_sequence = COALESCE(excluded.last_event_sequence, playback_sessions.last_event_sequence)",
        params![
            event.user_id.to_string(),
            event.device_id,
            media_id.to_string(),
            unit.season,
            unit.episode,
            event.client,
            event.device_name,
            event.client_version,
            play_method,
            position_ms,
            start_position_ms,
            duration_ms,
            i64::from(event.paused),
            watched_ms,
            event.rate_bps,
            started_at,
            now_ms / 1000,
            event.source_id.file_id.to_string(),
            event.source_id.revision,
            event.source_version,
            binding.library_id,
            binding.config_revision,
            "full_file",
            event.session_id,
            event.generation,
            if event.position_ms.is_some() { now_ms } else { 0 },
            event.event_sequence
        ],
    )?;
    Ok(())
}

/// Watch time this heartbeat may claim: elapsed wall time, capped by the position advance so a
/// seek never counts as watching.
fn watch_increment(
    previous: Option<&crate::playback::SessionRow>,
    event: &BoundPlaybackEvent,
    now_ms: i64,
) -> i64 {
    let (Some(previous), Some(position_ms)) = (previous, event.position_ms) else {
        return 0;
    };
    if previous.paused
        || previous.last_report_at_ms <= 0
        || (event.paused && event.event_type != PlaybackEventType::Stopped)
    {
        return 0;
    }
    let elapsed = (now_ms - previous.last_report_at_ms).clamp(0, 30_000);
    if elapsed == 0 {
        return 0;
    }
    let advance = (position_ms - previous.position_ms).max(0);
    let rate = if (0.25..=4.0).contains(&event.playback_rate) {
        event.playback_rate
    } else {
        1.0
    };
    let allowed = (elapsed as f64 * rate) as i64 + 2_000;
    if advance > allowed {
        return 0;
    }
    elapsed.min((advance as f64 / rate) as i64).max(0)
}
