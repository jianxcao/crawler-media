//! Session-bound playback event commit.
//!
//! A playback event is only allowed to touch the target and physical source that were
//! *issued* to that session. Before writing anything, the commit re-checks:
//!
//! * the `playback_session_ids` row exists for `(user, device, session)` and still carries
//!   the generation, target unit, source version and file revision the caller claims;
//! * the session is not already closed (a repeated `Stop` changes nothing);
//! * a supplied event sequence is newer than this session's last committed sequence;
//! * the file is still the published, present generation we issued, and it still covers the
//!   event's unit (two episodes can share one physical file);
//! * the Library is still `ready` at the config revision recorded at issuance, and the user
//!   may still read that Library.
//!
//! Anything else is `IgnoredStale`: the protocol may answer success, but no fact is written
//! and no session is closed. This is what keeps a late event for a replaced source, or for
//! another episode of the same file, from leaking into the wrong unit or session.

use domain::{MediaId, TypedItemId, UnitKey};
use rusqlite::{OptionalExtension, params};

use super::types::{BoundPlaybackEvent, EventOutcome, PlaybackEventType};
use super::{PlayLogRow, Store, StoreError, UnitState};

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
    last_event_sequence: Option<i64>,
}

/// How long after an admin ends a device's playback a later heartbeat still counts as
/// belonging to the ended session. Same window the HTTP route used before this guard moved
/// into the transaction that owns the session state.
const ADMIN_ENDED_WINDOW_SECS: i64 = 10;

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
        self.commit_playback_event_observed(event, now_ms, || {})
    }

    /// Test seam: `before_writer` runs after the pre-writer observation and
    /// immediately before acquiring the Library writer.
    #[doc(hidden)]
    pub fn commit_playback_event_observed(
        &self,
        event: &BoundPlaybackEvent,
        now_ms: i64,
        before_writer: impl FnOnce(),
    ) -> Result<EventOutcome, StoreError> {
        match self.with_verified_actor_library_write_observed(
            event.user_id,
            Some(&event.device_id),
            before_writer,
            |db| db.commit_playback_event(event, now_ms),
        )? {
            crate::connections::VerifiedActorOutcome::Applied(outcome) => Ok(outcome),
            crate::connections::VerifiedActorOutcome::InactiveUser
            | crate::connections::VerifiedActorOutcome::RevokedDevice => {
                Ok(EventOutcome::IgnoredStale)
            }
        }
    }
}

/// Guards for an event whose session id was issued: it must describe the generation, unit and
/// source the session was issued for, and that source and the library must still be readable.
///
/// Returns `true` when the event must be ignored as stale (nothing written).
fn stale_bound_event(
    store: &crate::LibraryConnection<'_>,
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    media_id: MediaId,
    unit: UnitKey,
) -> Result<bool, StoreError> {
    if !binding_matches(binding, event, media_id, unit) {
        tracing::debug!(
            session_id = %event.session_id,
            generation = event.generation,
            "playback event does not match its issued binding"
        );
        return Ok(true);
    }
    if !source_still_current(tx, event)? {
        tracing::debug!(
            session_id = %event.session_id,
            file_id = %event.source_id.file_id,
            "playback event for a replaced source ignored"
        );
        return Ok(true);
    }
    Ok(!library_still_readable(tx, store, event, binding)?)
}

/// Close the issued session of an admin-ended device without writing any unit fact.
fn archive_admin_ended_session(
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
    now_ms: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE playback_session_ids SET state = 'closed'
         WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
        params![event.user_id.to_string(), event.device_id, event.session_id],
    )?;
    session::close_live_session(tx, event, binding, None, now_ms)
}

/// A completed stop is the only event that counts as one play.
fn bump_play_count_for_completed_stop(
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    media_id: MediaId,
    unit: UnitKey,
    position_ms: i64,
    duration: Option<i64>,
) -> Result<(), StoreError> {
    if UnitState::playback_completed(position_ms, duration) {
        tx.execute(
            "UPDATE playback_units SET play_count = play_count + 1 WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
            params![event.user_id.to_string(), media_id.to_string(), unit.season, unit.episode],
        )?;
    }
    Ok(())
}

fn read_binding(
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
) -> Result<Option<IssuedBinding>, StoreError> {
    let row = tx
        .query_row(
            "SELECT generation, state, file_id, file_revision, source_version, library_id,
                    config_revision, media_id, season, episode, last_event_sequence
             FROM playback_session_ids
             WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
            params![event.user_id.to_string(), event.device_id, event.session_id],
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
                    last_event_sequence: r.get(10)?,
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
    tx: &rusqlite::Connection,
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
    tx: &rusqlite::Connection,
    store: &crate::LibraryConnection<'_>,
    event: &BoundPlaybackEvent,
    binding: &IssuedBinding,
) -> Result<bool, StoreError> {
    let role = store.user_role_observation(event.user_id)?;
    let row: Option<(String, i64, String, bool, String, bool)> = tx
        .query_row(
            "SELECT l.lifecycle_state, l.config_revision, l.access_mode, l.admin_visible,
                    l.member_ids_json,
                    EXISTS(SELECT 1 FROM library_removals r
                           WHERE r.library_id = l.library_id AND r.state != 'succeeded')
             FROM libraries l
             WHERE l.library_id = ?1",
            params![binding.library_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get(4)?,
                    r.get::<_, i64>(5)? != 0,
                ))
            },
        )
        .optional()?;
    let Some((lifecycle, config_revision, access_mode, admin_visible, members_json, removed)) = row
    else {
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
    if removed
        || !crate::library_read::library_access_permitted(
            event.user_id,
            role,
            &access_mode,
            admin_visible,
            &members_json,
        )
    {
        tracing::debug!(
            session_id = %event.session_id,
            library_id = %binding.library_id,
            user_id = %event.user_id,
            "playback event ignored: user no longer has library access"
        );
        return Ok(false);
    }
    Ok(true)
}

/// Write the unit progress/played facts the event carries.
///
/// Progress events never bump `play_count`: only a real terminal stop is a play. The clock is
/// `now_ms`, and a seek (a position jump beyond the elapsed time) never counts as watch time.
fn write_unit_facts(
    store: &crate::LibraryConnection<'_>,
    tx: &rusqlite::Connection,
    event: &BoundPlaybackEvent,
    media_id: MediaId,
    unit: UnitKey,
    position_ms: i64,
    duration: Option<i64>,
    now_ms: i64,
) -> Result<(), StoreError> {
    let tracks_version = store.file_tracks_version(event.source_id.file_id)?;
    // One duration decides both the stored package and completion (C06).
    let reached_played = UnitState::playback_completed(position_ms, duration);

    tx.execute(
        "INSERT INTO playback_units
            (user_id, media_id, season, episode, played, has_played_assertion,
             played_changed_at_ms, position_ms, duration_ms, resume_file_id,
             resume_file_revision, resume_source_version, timeline_mode,
             watch_activity_at, tracks_version, audio_track, subtitle_track)
         VALUES
            (?1, ?2, ?3, ?4, ?5, ?5, CASE WHEN ?5 = 1 THEN ?6 ELSE NULL END,
             ?7, ?8, ?9, ?10, ?11, 'full_file', ?15, ?12,
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
             watch_activity_at = ?15,
             tracks_version = ?12,
             audio_track = CASE
                 WHEN ?12 IS NULL THEN NULL
                 WHEN ?13 IS NOT NULL THEN ?13
                 WHEN playback_units.resume_file_id = ?9
                      AND playback_units.resume_file_revision = ?10
                      AND playback_units.resume_source_version = ?11
                      AND playback_units.timeline_mode = 'full_file'
                      AND playback_units.tracks_version = ?12
                 THEN playback_units.audio_track ELSE NULL END,
             subtitle_track = CASE
                 WHEN ?12 IS NULL THEN NULL
                 WHEN ?14 IS NOT NULL THEN ?14
                 WHEN playback_units.resume_file_id = ?9
                      AND playback_units.resume_file_revision = ?10
                      AND playback_units.resume_source_version = ?11
                      AND playback_units.timeline_mode = 'full_file'
                      AND playback_units.tracks_version = ?12
                 THEN playback_units.subtitle_track ELSE NULL END",
        params![
            event.user_id.to_string(),
            media_id.to_string(),
            unit.season,
            unit.episode,
            if reached_played { 1 } else { 0 },
            now_ms,
            position_ms,
            duration,
            event.source_id.file_id.to_string(),
            event.source_id.revision,
            event.source_version,
            tracks_version,
            event.audio_track,
            event.subtitle_track,
            // Activity columns are seconds; only the played clock is milliseconds.
            now_ms / 1000,
        ],
    )?;
    Ok(())
}

/// Did an admin end this device's playback, so a later event must not restart it?
///
/// The live row or recent archived log must match this exact issued session generation. A
/// newer session on the same device is unaffected by an older session's admin-end marker. A
/// `start` may only be stopped by its own still-live admin-ended row; archived-session markers
/// apply to later progress/stop events.
fn admin_ended_session(
    store: &crate::LibraryConnection<'_>,
    event: &BoundPlaybackEvent,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let live = store.live_play_session_was_admin_ended(
        event.user_id,
        &event.device_id,
        &event.session_id,
        event.generation,
    )?;
    let archived = event.event_type != PlaybackEventType::Playing
        && store.archived_play_session_was_admin_ended(
            event.user_id,
            &event.device_id,
            &event.session_id,
            event.generation,
            now_ms / 1000,
            ADMIN_ENDED_WINDOW_SECS,
        )?;
    Ok(live || archived)
}

#[path = "event/connection.rs"]
mod connection;
#[path = "event/session.rs"]
mod session;

use session::advance_session_state;
