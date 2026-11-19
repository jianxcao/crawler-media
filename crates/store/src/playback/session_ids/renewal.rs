use domain::UserId;
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum PlaybackRenewalError {
    #[error("previous playback session unavailable")]
    PreviousUnavailable,
    #[error("previous playback target changed")]
    TargetChanged,
    #[error("playback superseded by a newer session")]
    Superseded,
    #[error("playback ended by administrator")]
    EndedByAdmin,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl Store {
    /// Renew a saved page's grant only while its exact predecessor remains authoritative.
    pub fn renew_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        previous_session_id: &str,
        resolved: &crate::ResolvedResume,
        now: i64,
    ) -> Result<String, PlaybackRenewalError> {
        let role = self
            .user_role(user_id)?
            .parse()
            .map_err(StoreError::Invalid)?;
        self.library_write_with_error(PlaybackRenewalError::Store, |db| {
            validate_previous(
                db.library,
                user_id,
                device_id,
                previous_session_id,
                resolved,
            )?;
            let target = &resolved.target;
            super::connection::issue_on(
                db.library,
                user_id,
                device_id,
                target.file.file_id,
                target.media_id,
                target.unit,
                &target.file.version,
                now,
                role,
                Some(target.config_revision),
            )
            .map_err(Into::into)
        })
    }
}

fn validate_previous(
    conn: &rusqlite::Connection,
    user: UserId,
    device: &str,
    previous: &str,
    resolved: &crate::ResolvedResume,
) -> Result<(), PlaybackRenewalError> {
    let previous_binding: Option<(String, i32, i32, i64)> = conn
        .query_row(
            "SELECT media_id, season, episode, generation FROM playback_session_ids
         WHERE user_id=?1 AND device_id=?2 AND play_session_id=?3",
            params![user.to_string(), device, previous],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(StoreError::from)?;
    let (media, season, episode, generation) =
        previous_binding.ok_or(PlaybackRenewalError::PreviousUnavailable)?;
    if media != resolved.target.media_id.to_string()
        || domain::UnitKey::new(season, episode) != resolved.target.unit
    {
        return Err(PlaybackRenewalError::TargetChanged);
    }
    let admin_ended: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM playback_sessions
           WHERE user_id=?1 AND device_id=?2 AND play_session_id=?3 AND admin_ended=1)
         OR EXISTS(SELECT 1 FROM playback_logs
           WHERE user_id=?1 AND device_id=?2 AND play_session_id=?3 AND admin_ended=1)",
            params![user.to_string(), device, previous],
            |row| row.get(0),
        )
        .map_err(StoreError::from)?;
    if admin_ended {
        return Err(PlaybackRenewalError::EndedByAdmin);
    }
    let superseded: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM playback_session_ids
          WHERE user_id=?1 AND device_id=?2 AND generation>?3)",
            params![user.to_string(), device, generation],
            |row| row.get(0),
        )
        .map_err(StoreError::from)?;
    if superseded {
        return Err(PlaybackRenewalError::Superseded);
    }
    Ok(())
}
