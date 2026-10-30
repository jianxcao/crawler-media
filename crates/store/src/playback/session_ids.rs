use std::str::FromStr;

use domain::{LedgerId, UserId};
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaySessionIdState {
    Pending,
    Active,
    Closed,
}

impl PlaySessionIdState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Closed => "closed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaySessionBinding {
    pub ledger_id: LedgerId,
    pub issued_at: i64,
    pub generation: i64,
    pub state: PlaySessionIdState,
    pub media_id: domain::MediaId,
    pub unit: domain::UnitKey,
    pub file_id: Option<String>,
    pub file_revision: Option<i64>,
    pub source_version: Option<String>,
    pub library_id: Option<String>,
    pub config_revision: i64,
}

impl Store {
    /// Issue a new play session ID for a (user, device, ledger) combination.
    ///
    /// Generates a UUID v4 ID, determines the next generation atomically in the
    /// library.db immediate transaction, cleans expired IDs (up to 500), and records the pending ID.
    pub fn issue_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        ledger_id: LedgerId,
        media_id: domain::MediaId,
        unit: domain::UnitKey,
        version: &domain::FileVersion,
        now: i64,
    ) -> Result<String, StoreError> {
        self.with_library_write(|db| {
            db.issue_play_session(user_id, device_id, ledger_id, media_id, unit, version, now)
        })
    }

    /// Retrieve binding info for a session ID given user and device.
    pub fn play_session_binding(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<Option<PlaySessionBinding>, StoreError> {
        self.with_library_read(|db| db.play_session_binding(user_id, device_id, play_session_id))
    }

    /// Activate a play session ID.
    ///
    /// If the ID is already active, returns Ok(true) idempotently.
    /// If pending, verifies that no higher generation session has been activated on the device,
    /// marks older pending/active sessions on this device as closed, and marks this one active.
    /// If closed or unknown, returns Ok(false).
    ///
    /// A refused activation writes nothing: neither the `closed`/unknown row nor an older
    /// generation than the active session is touched.
    pub fn activate_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.activate_play_session(user_id, device_id, play_session_id))
    }

    /// Mark a play session ID as closed.
    pub fn close_play_session_id(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.close_play_session_id(user_id, device_id, play_session_id))
    }
}

#[path = "session_ids/connection.rs"]
mod connection;
