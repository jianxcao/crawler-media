use domain::UserId;
use rusqlite::{OptionalExtension, params};

use super::{PlayLogRow, SessionRow, UnitState, media_id_from, user_id_from};
use super::{Store, StoreError};

/// The NOT NULL binding columns a live session shares with the play session id
/// it was issued for. `playback_session_ids` is the API-visible record of the
/// file, revision, library and generation the client is playing, so a session
/// row must echo exactly that state instead of inventing defaults.
struct SessionBinding {
    play_session_id: String,
    generation: i64,
    file_id: String,
    file_revision: i64,
    source_version: String,
    library_id: String,
    config_revision: i64,
    timeline_mode: String,
}

impl Store {
    pub fn upsert_session(&self, session: &SessionRow) -> Result<(), StoreError> {
        self.with_library_write(|db| db.upsert_session(session))
    }

    pub fn get_session(
        &self,
        user_id: UserId,
        device_id: &str,
    ) -> Result<Option<SessionRow>, StoreError> {
        self.with_library_read(|db| db.get_session(user_id, device_id))
    }

    pub fn sessions_for_device(&self, device_id: &str) -> Result<Vec<SessionRow>, StoreError> {
        self.with_library_read(|db| db.sessions_for_device(device_id))
    }
    pub fn active_sessions(
        &self,
        now: i64,
        timeout_secs: i64,
    ) -> Result<Vec<SessionRow>, StoreError> {
        self.with_library_read(|db| db.active_sessions(now, timeout_secs))
    }

    /// Close a session into a play log; returns the log row. Missing session = None.
    pub fn close_session(
        &self,
        user_id: UserId,
        device_id: &str,
        now: i64,
    ) -> Result<Option<PlayLogRow>, StoreError> {
        self.with_library_write(|db| db.close_session(user_id, device_id, now))
    }

    pub fn end_device_session(
        &self,
        user_id: UserId,
        device_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.end_device_session(user_id, device_id, now))
    }

    /// Check if a session was explicitly ended by admin on this device within window_secs.
    pub fn device_was_admin_ended(
        &self,
        user_id: UserId,
        device_id: &str,
        now: i64,
        window_secs: i64,
    ) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.device_was_admin_ended(user_id, device_id, now, window_secs))
    }

    /// Close every session whose heartbeat is older than the timeout.
    pub fn sweep_stale_sessions(&self, now: i64, timeout_secs: i64) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.sweep_stale_sessions(now, timeout_secs))
    }
}

pub(super) fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        device_id: row.get(0)?,
        user_id: user_id_from(row, 1)?,
        media_id: media_id_from(row, 2)?,
        season: row.get(3)?,
        episode: row.get(4)?,
        client: row.get(5)?,
        device_name: row.get(6)?,
        client_version: row.get(7)?,
        play_method: row.get(8)?,
        position_ms: row.get(9)?,
        start_position_ms: row.get(10)?,
        duration_ms: row.get(11)?,
        paused: row.get::<_, i64>(12)? != 0,
        watched_ms: row.get(13)?,
        rate_bps: row.get(14)?,
        bytes_sent: row.get(15)?,
        connections: row.get(16)?,
        admin_ended: row.get::<_, i64>(17)? != 0,
        started_at: row.get(18)?,
        last_report_at: row.get(19)?,
        ledger_id: row.get(20)?,
        play_session_id: row.get(21)?,
        last_report_at_ms: row.get(22)?,
        last_event_sequence: row.get(23)?,
    })
}

#[path = "sessions/connection.rs"]
mod connection;
