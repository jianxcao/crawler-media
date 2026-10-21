//! Session-bound playback event commit.
//!
//! A playback event is only allowed to touch the target and physical source that were
//! *issued* to that session. Before writing anything, the commit re-checks:
//!
//! * the `playback_session_ids` row exists for `(user, device, session)` and still carries
//!   the generation, target unit, source version and file revision the caller claims;
//! * the session is not already closed (a repeated `Stop` changes nothing);
//! * the file is still the published, present generation we issued, and it still covers the
//!   event's unit (two episodes can share one physical file);
//! * the Library is still `ready` at the config revision recorded at issuance, and the user
//!   may still read that Library.
//!
//! Anything else is `IgnoredStale`: the protocol may answer success, but no fact is written
//! and no session is closed. This is what keeps a late event for a replaced source, or for
//! another episode of the same file, from leaking into the wrong unit or session.

use domain::{MediaId, TypedItemId, UnitKey, UserId};
use rusqlite::{OptionalExtension, Transaction, params};

use super::types::{BoundPlaybackEvent, EventOutcome, PlaybackEventType};
use super::user_summary::SummaryScope;
use super::{PlayLogRow, SessionRow, Store, StoreError, UnitState};

/// What the session was issued for.
struct IssuedBinding {
    generation: i64,
    state: String,
    file_id: String,
    file_revision: i64,
    source_version: String,
    library_id: String,
    config_revision: i64,
    media_id: String,
    season: i32,
    episode: i32,
}

/// Watch unit coordinates of a logical item.
fn playback_unit(item: &TypedItemId) -> (MediaId, UnitKey) {
    match item {
        TypedItemId::Work(media) => (*media, UnitKey::WHOLE),
        TypedItemId::Season { media, season } => (*media, UnitKey::new(*season as i32, 1)),
        TypedItemId::Unit { media, key } => (*media, *key),
    }
}

impl Store {
    /// Commit one playback event for the session that was issued for it.
    ///
    /// Returns `IgnoredStale` without writing when the event does not belong to the issued
    /// generation, target or source, or when the Library/source generation moved on.
    pub fn commit_playback_event(
        &self,
        event: &BoundPlaybackEvent,
        now_ms: i64,
    ) -> Result<EventOutcome, StoreError> {
        let (media_id, unit) = playback_unit(&event.item);
        let tx = self.library.unchecked_transaction()?;
        let Some(binding) = read_binding(&tx, event)? else {
            // Legacy clients never issued a session id: they still report progress, so the
            // unit facts are written, but no session state is invented for them. Once a
            // session *was* issued, the checks below decide and a mismatch writes nothing.
            if !source_still_current(&tx, event)? {
                return Ok(EventOutcome::IgnoredStale);
            }
            tracing::debug!(
                user_id = %event.user_id,
                device_id = %event.device_id,
                session_id = %event.session_id,
                "unbound playback event: writing unit facts only"
            );
            write_unit_facts(self, &tx, event, media_id, unit, now_ms)?;
            self.refresh_user_library_summary(
                event.user_id,
                media_id,
                SummaryScope::for_unit(unit.season, unit.episode),
            )?;
            tx.commit()?;
            return Ok(EventOutcome::Applied);
        };
        if !binding_matches(&binding, event, media_id, unit) {
            tracing::debug!(
                session_id = %event.session_id,
                generation = event.generation,
                "playback event does not match its issued binding"
            );
            return Ok(EventOutcome::IgnoredStale);
        }
        if !source_still_current(&tx, event)? {
            tracing::debug!(
                session_id = %event.session_id,
                file_id = %event.source_id.file_id,
                "playback event for a replaced source ignored"
            );
            return Ok(EventOutcome::IgnoredStale);
        }
        if !library_still_readable(&tx, self, event, &binding)? {
            return Ok(EventOutcome::IgnoredStale);
        }
        // Starting or progressing activates exactly this session; a session already closed by
        // a newer generation on the same device is refused instead of being reused.
        if event.event_type != PlaybackEventType::Stopped
            && !self.activate_play_session(event.user_id, &event.device_id, &event.session_id)?
        {
            return Ok(EventOutcome::IgnoredStale);
        }

        write_unit_facts(self, &tx, event, media_id, unit, now_ms)?;
        advance_session_state(self, &tx, event, &binding, now_ms)?;

        self.refresh_user_library_summary(
            event.user_id,
            media_id,
            SummaryScope::for_unit(unit.season, unit.episode),
        )?;
        tx.commit()?;
        tracing::debug!(
            session_id = %event.session_id,
            event = ?event.event_type,
            position_ms = event.position_ms,
            "playback event applied"
        );
        Ok(EventOutcome::Applied)
    }
}

fn read_binding(
    tx: &Transaction<'_>,
    event: &BoundPlaybackEvent,
) -> Result<Option<IssuedBinding>, StoreError> {
    let row = tx
        .query_row(
            "SELECT generation, state, file_id, file_revision, source_version, library_id,
                    config_revision, media_id, season, episode
             FROM playback_session_ids
             WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
            params![
                event.user_id.to_string(),
                event.device_id,
                event.session_id
            ],
            |r| {
                Ok(IssuedBinding {
                    generation: r.get(0)?,
                    state: r.get(1)?,
                    file_id: r.get(2)?,
                    file_revision: r.get(3)?,
                    source_version: r.get(4)?,
                    library_id: r.get(5)?,
                    config_revision: r.get(6)?,
                    media_id: r.get(7)?,
                    season: r.get(8)?,
                    episode: r.get(9)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// The event must describe the generation, unit and source the session was issued for.
fn binding_matches(
    binding: &IssuedBinding,
    event: &BoundPlaybackEvent,
    media_id: MediaId,
    unit: UnitKey,
) -> bool {
    binding.state != "closed"
        && binding.generation == event.generation
        && binding.file_id == event.source_id.file_id.to_string()
        && binding.file_revision == event.source_id.revision
        && binding.source_version == event.source_version
        && binding.media_id == media_id.to_string()
        && binding.season == unit.season
        && binding.episode == unit.episode
}

/// The issued physical source must still be the published, present generation that covers the
/// event's unit. A replacement scan that bumped the revision makes the old event stale.
fn source_still_current(
    tx: &Transaction<'_>,
    event: &BoundPlaybackEvent,
) -> Result<bool, StoreError> {
    let file: Option<(String, i64, String, String, String, String)> = tx
        .query_row(
            "SELECT library_id, file_revision, source_version, presence_state, publication_state,
                    media_id
             FROM library_files WHERE file_id = ?1",
            params![event.source_id.file_id.to_string()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((_, revision, source_version, presence, publication, _media)) = file else {
        return Ok(false);
    };
    if presence != "present" || publication != "published" {
        return Ok(false);
    }
    if revision != event.source_id.revision || source_version != event.source_version {
        return Ok(false);
    }
    let (media_id, unit) = playback_unit(&event.item);
    let covers: i64 = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_file_units
                       WHERE file_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4)",
        params![
            event.source_id.file_id.to_string(),
            media_id.to_string(),
            unit.season,
            unit.episode
        ],
        |r| r.get(0),
    )?;
    Ok(covers != 0)
}

/// The Library recorded at issuance must still be `ready`, at the same config revision, and
/// readable by this user.
fn library_still_readable(
    tx: &Transaction<'_>,
    store: &Store,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
) -> Result<bool, StoreError> {
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT lifecycle_state, config_revision FROM libraries WHERE library_id = ?1",
            params![binding.library_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((lifecycle, config_revision)) = row else {
        return Ok(false);
    };
    if lifecycle != "ready" || config_revision != binding.config_revision {
        tracing::debug!(
            session_id = %event.session_id,
            library_id = %binding.library_id,
            lifecycle,
            "playback event ignored: library generation changed"
        );
        return Ok(false);
    }
    let allowed = store.authorized_library_ids(event.user_id)?;
    Ok(allowed.contains(&binding.library_id))
}

/// Write the unit progress/played facts the event carries.
///
/// Progress events never bump `play_count`: only a real terminal stop is a play. The clock is
/// `now_ms`, and a seek (a position jump beyond the elapsed time) never counts as watch time.
fn write_unit_facts(
    store: &Store,
    tx: &Transaction<'_>,
    event: &BoundPlaybackEvent,
    media_id: MediaId,
    unit: UnitKey,
    now_ms: i64,
) -> Result<(), StoreError> {
    let tracks_version = store.file_tracks_version(event.source_id.file_id)?;
    let played_threshold = event
        .duration_ms
        .map(|d| (d as f64 * 0.9) as i64)
        .unwrap_or(i64::MAX);
    let reached_played = event.position_ms >= played_threshold;

    tx.execute(
        "INSERT INTO playback_units
            (user_id, media_id, season, episode, played, has_played_assertion,
             played_changed_at_ms, position_ms, duration_ms, resume_file_id,
             resume_file_revision, resume_source_version, timeline_mode,
             watch_activity_at, tracks_version, audio_track, subtitle_track)
         VALUES
            (?1, ?2, ?3, ?4, ?5, ?5, CASE WHEN ?5 = 1 THEN ?6 ELSE NULL END,
             ?7, ?8, ?9, ?10, ?11, 'full_file', ?6, ?12,
             CASE WHEN ?12 IS NULL THEN NULL ELSE ?13 END,
             CASE WHEN ?12 IS NULL THEN NULL ELSE ?14 END)
         ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
             played = CASE WHEN ?5 = 1 THEN 1 ELSE playback_units.played END,
             has_played_assertion = CASE WHEN ?5 = 1 THEN 1 ELSE playback_units.has_played_assertion END,
             played_changed_at_ms = CASE WHEN ?5 = 1 THEN ?6 ELSE playback_units.played_changed_at_ms END,
             position_ms = ?7,
             duration_ms = ?8,
             resume_file_id = ?9,
             resume_file_revision = ?10,
             resume_source_version = ?11,
             timeline_mode = 'full_file',
             watch_activity_at = ?6,
             tracks_version = ?12,
             audio_track = CASE WHEN ?12 IS NULL THEN playback_units.audio_track WHEN ?13 IS NOT NULL THEN ?13 ELSE playback_units.audio_track END,
             subtitle_track = CASE WHEN ?12 IS NULL THEN playback_units.subtitle_track WHEN ?14 IS NOT NULL THEN ?14 ELSE playback_units.subtitle_track END",
        params![
            event.user_id.to_string(),
            media_id.to_string(),
            unit.season,
            unit.episode,
            if reached_played { 1 } else { 0 },
            now_ms,
            event.position_ms,
            event.duration_ms,
            event.source_id.file_id.to_string(),
            event.source_id.revision,
            event.source_version,
            tracks_version,
            event.audio_track,
            event.subtitle_track,
        ],
    )?;
    Ok(())
}

fn close_live_session(
    tx: &Transaction<'_>,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    now_ms: i64,
) -> Result<(), StoreError> {
    let session: Option<super::SessionRow> = tx
        .query_row(
            "SELECT device_id, user_id, media_id, season, episode, client, device_name,
                    client_version, play_method, position_ms, start_position_ms,
                    duration_ms, paused, watched_ms, rate_bps, bytes_sent, connections,
                    admin_ended, started_at, last_report_at, file_id, play_session_id,
                    last_report_at_ms, last_event_sequence
             FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
            params![event.user_id.to_string(), event.device_id],
            super::sessions::map_session,
        )
        .optional()?;

    let Some(session) = session else {
        return Ok(());
    };

    tx.execute(
        "DELETE FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
        params![event.user_id.to_string(), event.device_id],
    )?;

    let now = now_ms / 1000;
    let completed = UnitState::playback_completed(session.position_ms, session.duration_ms);
    let log = PlayLogRow {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: session.user_id,
        media_id: session.media_id,
        season: session.season,
        episode: session.episode,
        device_id: Some(session.device_id),
        client: session.client,
        device_name: session.device_name,
        play_method: session.play_method,
        started_at: session.started_at,
        ended_at: now,
        watched_ms: session.watched_ms,
        start_position_ms: session.start_position_ms,
        end_position_ms: session.position_ms,
        duration_ms: session.duration_ms,
        completed,
    };

    tx.execute(
        "INSERT INTO playback_logs
            (id, user_id, media_id, season, episode, client, device_name, play_method,
             started_at, ended_at, watched_ms, start_position_ms, end_position_ms,
             duration_ms, completed, device_id, admin_ended, play_session_id, generation,
             file_id, file_revision, source_version, timeline_mode)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
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
            event.session_id, // play_session_id
            binding.generation,
            binding.file_id,
            binding.file_revision,
            binding.source_version,
            "full_file",
        ],
    )?;
    Ok(())
}

/// A `Stop` closes exactly the session that reported it and drops its live session row; a
/// repeated `Stop` never reaches here because the binding is already `closed`.
fn advance_session_state(
    store: &Store,
    tx: &Transaction<'_>,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    now_ms: i64,
) -> Result<(), StoreError> {
    let previous = store.get_session(event.user_id, &event.device_id)?;
    if event.event_type == PlaybackEventType::Stopped
        || previous.as_ref().is_some_and(|s| s.admin_ended)
    {
        // The event was validated against the issued binding, so the live row that belongs to
        // (user, device) is this session: archiving it writes exactly one play log.
        tx.execute(
            "UPDATE playback_session_ids SET state = 'closed'
             WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
            params![event.user_id.to_string(), event.device_id, event.session_id],
        )?;
        close_live_session(tx, event, binding, now_ms)?;
        return Ok(());
    }
    upsert_live_session(store, tx, event, binding, now_ms)
}

/// Create or advance the live `playback_sessions` row for this session.
///
/// `started_at`, `start_position_ms` and `watched_ms` are carried forward so a heartbeat is an
/// update of one session, not a new one; a seek that jumps ahead of the elapsed time adds no
/// watch time.
fn upsert_live_session(
    store: &Store,
    tx: &Transaction<'_>,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    now_ms: i64,
) -> Result<(), StoreError> {
    let previous = store.get_session(event.user_id, &event.device_id)?;
    let started_at = previous
        .as_ref()
        .map(|session| session.started_at)
        .unwrap_or(now_ms / 1000);
    let start_position_ms = previous
        .as_ref()
        .map(|session| session.start_position_ms)
        .unwrap_or(event.position_ms);
    let watched_ms = previous
        .as_ref()
        .map(|session| session.watched_ms)
        .unwrap_or(0)
        + watch_increment(previous.as_ref(), event, now_ms);
    let play_method = previous
        .as_ref()
        .map(|session| session.play_method.clone())
        .unwrap_or_else(|| "local".to_string());
    let (media_id, unit) = playback_unit(&event.item);
    tx.execute(
        "INSERT INTO playback_sessions
            (user_id, device_id, media_id, season, episode, client, play_method, position_ms,
             start_position_ms, duration_ms, paused, watched_ms, rate_bps, started_at,
             last_report_at, file_id, file_revision, source_version, library_id,
             config_revision, timeline_mode, play_session_id, generation, last_report_at_ms,
             last_event_sequence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)
         ON CONFLICT(user_id, device_id) DO UPDATE SET
            media_id = excluded.media_id, season = excluded.season, episode = excluded.episode,
            client = COALESCE(excluded.client, playback_sessions.client),
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
            play_method,
            event.position_ms,
            start_position_ms,
            event.duration_ms,
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
            now_ms,
            event.event_sequence
        ],
    )?;
    Ok(())
}

/// Watch time this heartbeat may claim: elapsed wall time, capped by the position advance so a
/// seek never counts as watching.
fn watch_increment(previous: Option<&super::SessionRow>, event: &BoundPlaybackEvent, now_ms: i64) -> i64 {
    let Some(previous) = previous else {
        return 0;
    };
    if previous.paused || previous.last_report_at_ms <= 0 {
        return 0;
    }
    let elapsed = (now_ms - previous.last_report_at_ms).clamp(0, 30_000);
    if elapsed == 0 {
        return 0;
    }
    let advance = (event.position_ms - previous.position_ms).max(0);
    let rate = if (0.25..=4.0).contains(&(event.rate_bps as f64)) {
        event.rate_bps as f64
    } else {
        1.0
    };
    let allowed = (elapsed as f64 * rate) as i64 + 2_000;
    if advance > allowed {
        return 0;
    }
    elapsed.min((advance as f64 / rate) as i64).max(0)
}

fn binding_library_placeholder() -> String {
    String::new()
}
