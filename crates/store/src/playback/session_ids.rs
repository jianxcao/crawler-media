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
        let play_session_id = uuid::Uuid::new_v4().to_string();
        let user_str = user_id.to_string();
        let item_id_str = ledger_id.to_string();

        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        let cutoff = now - 2_592_000;
        tx.execute(
            "DELETE FROM playback_session_ids
             WHERE rowid IN (
                 SELECT rowid FROM playback_session_ids
                 WHERE issued_at < ?1 AND state IN ('pending', 'closed')
                 ORDER BY issued_at ASC LIMIT 500
             )",
            params![cutoff],
        )?;

        let max_gen: i64 = tx.query_row(
            "SELECT COALESCE(MAX(generation), 0) FROM playback_session_ids WHERE user_id = ?1 AND device_id = ?2",
            params![user_str, device_id],
            |r| r.get(0),
        )?;
        let generation = max_gen + 1;

        let file_info: Option<(String, String, i64, String)> = tx
            .query_row(
                "SELECT media_id, library_id, file_revision, source_version
                 FROM library_files WHERE file_id = ?1 AND presence_state = 'present'
                   AND publication_state = 'published'",
                params![item_id_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((file_media, library_id, revision, source_version)) = file_info else {
            return Err(StoreError::Missing(format!("playable file {ledger_id}")));
        };
        if file_media != media_id.to_string()
            || revision != version.revision
            || source_version != version.source_version
        {
            return Err(StoreError::Protected(format!(
                "playback source changed for {ledger_id}"
            )));
        }
        let coverage_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_file_units
             WHERE file_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4)",
            params![item_id_str, media_id.to_string(), unit.season, unit.episode],
            |r| r.get(0),
        )?;
        if !coverage_exists {
            return Err(StoreError::Missing(format!(
                "source does not cover {unit:?}"
            )));
        }
        let (config_revision, lifecycle): (i64, String) = tx.query_row(
            "SELECT config_revision, lifecycle_state FROM libraries WHERE library_id = ?1",
            params![library_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if lifecycle != "ready" {
            return Err(StoreError::Protected(format!(
                "library {library_id} is not ready"
            )));
        }

        tx.execute(
            "INSERT INTO playback_session_ids
                (user_id, device_id, play_session_id, item_id, media_id, season, episode,
                 file_id, file_revision, source_version, library_id, config_revision, timeline_mode,
                 issued_at, generation, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'full_file', ?13, ?14, 'pending')",
            params![user_str, device_id, play_session_id, item_id_str, media_id.to_string(),
                unit.season, unit.episode, item_id_str, version.revision, version.source_version,
                library_id, config_revision, now, generation],
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
                "SELECT item_id, issued_at, generation, state, file_id, file_revision, source_version, library_id, config_revision, media_id, season, episode
                 FROM playback_session_ids
                 WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
                params![user_id.to_string(), device_id, play_session_id],
                |row| {
                    let item_str: String = row.get(0)?;
                    let issued_at: i64 = row.get(1)?;
                    let generation: i64 = row.get(2)?;
                    let state_str: String = row.get(3)?;
                    let file_id: Option<String> = row.get(4)?;
                    let file_revision: Option<i64> = row.get(5)?;
                    let source_version: Option<String> = row.get(6)?;
                    let library_id: Option<String> = row.get(7)?;
                    let config_revision: i64 = row.get(8)?;
                    let media_id: String = row.get(9)?;
                    let season: i32 = row.get(10)?;
                    let episode: i32 = row.get(11)?;
                    let ledger_id = LedgerId::from_str(&item_str)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                    let state = PlaySessionIdState::from_str(&state_str)
                        .unwrap_or(PlaySessionIdState::Closed);
                    Ok(PlaySessionBinding {
                        ledger_id,
                        issued_at,
                        generation,
                        state,
                        file_id,
                        file_revision,
                        source_version,
                        library_id,
                        config_revision,
                        media_id: media_id.parse().map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, 0))?,
                        unit: domain::UnitKey::new(season, episode),
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
    ///
    /// A refused activation writes nothing: neither the `closed`/unknown row nor an older
    /// generation than the active session is touched.
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

        if let Some(active_gen) = max_active_gen
            && active_gen >= generation {
                // 被拒绝的激活是零写入:这一行既不能置 `closed`,也不能有任何别的改动。
                // 它是否被清理交给后续成功的激活或 `close_play_session_id`。
                return Ok(false);
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
