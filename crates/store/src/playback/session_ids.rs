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
}

impl Store {
    /// Issue a new play session ID for a (user, device, ledger) combination.
    ///
    /// Generates a UUID v4 ID, determines the next generation atomically in the
    /// subscribe.db immediate transaction, cleans expired IDs (up to 500), and records the pending ID.
    pub fn issue_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        ledger_id: LedgerId,
        now: i64,
    ) -> Result<String, StoreError> {
        let play_session_id = uuid::Uuid::new_v4().to_string();
        let user_str = user_id.to_string();
        let item_id_str = ledger_id.to_string();

        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        // Clean expired sessions (older than 30 days = 30 * 86400 = 2_592_000 seconds)
        // only delete pending or closed, max 500
        let cutoff = now - 2_592_000;
        tx.execute(
            "DELETE FROM playback_session_ids
             WHERE rowid IN (
                 SELECT rowid FROM playback_session_ids
                 WHERE issued_at < ?1 AND state IN ('pending', 'closed')
                 ORDER BY issued_at ASC
                 LIMIT 500
             )",
            params![cutoff],
        )?;

        // Compute generation = MAX(generation) + 1 for (user_id, device_id)
        let max_gen: Option<i64> = tx
            .query_row(
                "SELECT MAX(generation) FROM playback_session_ids WHERE user_id = ?1 AND device_id = ?2",
                params![user_str, device_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        let generation = max_gen.unwrap_or(0) + 1;

        // Query target file binding from library_files and library_file_units
        let file_info: Option<(String, String, i32, i32, i64, String, String)> = tx.query_row(
            "SELECT f.media_id, f.library_id, COALESCE(u.season, -1), COALESCE(u.episode, -1),
                    f.file_revision, f.source_version, f.file_id
             FROM library_files f
             LEFT JOIN library_file_units u ON f.file_id = u.file_id
             WHERE f.file_id = ?1 LIMIT 1",
            params![item_id_str],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        ).optional()?;

        let (mid, lid, s, ep, rev, s_ver, fid) = match file_info {
            Some(info) => info,
            None => {
                // If not in library_files, treat item_id as file_id with default revision
                (item_id_str.clone(), "".into(), -1, -1, 1, "v1".into(), item_id_str.clone())
            }
        };

        tx.execute(
            "INSERT INTO playback_session_ids
                (user_id, device_id, play_session_id, item_id, media_id, season, episode,
                 file_id, file_revision, source_version, library_id, config_revision, timeline_mode,
                 issued_at, generation, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 'full_file', ?12, ?13, 'pending')",
            params![
                user_str,
                device_id,
                play_session_id,
                item_id_str,
                mid,
                s,
                ep,
                fid,
                rev,
                s_ver,
                lid,
                now,
                generation,
            ],
        )?;

        tx.commit()?;
        Ok(play_session_id)
    }

    /// Retrieve binding info for a session ID given user and device.
    pub fn play_session_binding(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<Option<PlaySessionBinding>, StoreError> {
        self.library
            .query_row(
                "SELECT item_id, issued_at, generation, state
                 FROM playback_session_ids
                 WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
                params![user_id.to_string(), device_id, play_session_id],
                |row| {
                    let item_str: String = row.get(0)?;
                    let issued_at: i64 = row.get(1)?;
                    let generation: i64 = row.get(2)?;
                    let state_str: String = row.get(3)?;
                    let ledger_id = LedgerId::from_str(&item_str)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                    let state = PlaySessionIdState::from_str(&state_str)
                        .unwrap_or(PlaySessionIdState::Closed);
                    Ok(PlaySessionBinding {
                        ledger_id,
                        issued_at,
                        generation,
                        state,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Activate a play session ID.
    ///
    /// If the ID is already active, returns Ok(true) idempotently.
    /// If pending, verifies that no higher generation session has been activated on the device,
    /// marks older pending/active sessions on this device as closed, and marks this one active.
    /// If closed or unknown, returns Ok(false).
    pub fn activate_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<bool, StoreError> {
        if !self.library.is_autocommit() {
            return self.activate_play_session_conn(
                &self.library,
                user_id,
                device_id,
                play_session_id,
            );
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let res = self.activate_play_session_conn(&tx, user_id, device_id, play_session_id)?;
        tx.commit()?;
        Ok(res)
    }

    fn activate_play_session_conn(
        &self,
        conn: &rusqlite::Connection,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<bool, StoreError> {
        let current: Option<(i64, String)> = conn
            .query_row(
                "SELECT generation, state
                 FROM playback_session_ids
                 WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
                params![user_id.to_string(), device_id, play_session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let Some((generation, state_str)) = current else {
            return Ok(false);
        };

        if state_str == "active" {
            return Ok(true);
        }
        if state_str != "pending" {
            return Ok(false);
        }

        let max_active_gen: Option<i64> = conn
            .query_row(
                "SELECT MAX(generation)
                 FROM playback_session_ids
                 WHERE user_id = ?1 AND device_id = ?2 AND state = 'active'",
                params![user_id.to_string(), device_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();

        if let Some(active_gen) = max_active_gen {
            if active_gen >= generation {
                conn.execute(
                    "UPDATE playback_session_ids SET state = 'closed'
                     WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
                    params![user_id.to_string(), device_id, play_session_id],
                )?;
                return Ok(false);
            }
        }

        conn.execute(
            "UPDATE playback_session_ids SET state = 'closed'
             WHERE user_id = ?1 AND device_id = ?2 AND generation < ?3 AND state IN ('pending', 'active')",
            params![user_id.to_string(), device_id, generation],
        )?;

        conn.execute(
            "UPDATE playback_session_ids SET state = 'active'
             WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
            params![user_id.to_string(), device_id, play_session_id],
        )?;

        Ok(true)
    }

    /// Mark a play session ID as closed.
    pub fn close_play_session_id(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE playback_session_ids SET state = 'closed'
             WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
            params![user_id.to_string(), device_id, play_session_id],
        )?;
        Ok(())
    }
}
