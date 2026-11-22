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
        let outcome = self.with_verified_actor_library_write(user_id, Some(device_id), |db| {
            let role = db.user_role_observation(user_id)?;
            let aliases = crate::path_key::AliasScope::new(db.library);
            let tx = crate::write_tx(db.library)?;
            let inner: Result<String, PlaybackRenewalError> = (|| {
                validate_previous(&tx, user_id, device_id, previous_session_id, resolved)?;
                let target = &resolved.target;
                super::connection::issue_on(
                    &tx,
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
            })();
            match inner {
                Ok(id) => {
                    tx.commit()?;
                    aliases.publish()?;
                    Ok(Ok(id))
                }
                Err(err) => Ok(Err(err)),
            }
        })?;
        match outcome {
            crate::connections::VerifiedActorOutcome::Applied(res) => res,
            crate::connections::VerifiedActorOutcome::InactiveUser => {
                Err(PlaybackRenewalError::Store(StoreError::Protected(format!(
                    "user {user_id} is inactive"
                ))))
            }
            crate::connections::VerifiedActorOutcome::RevokedDevice => {
                Err(PlaybackRenewalError::Store(StoreError::Protected(format!(
                    "device {device_id} is revoked"
                ))))
            }
        }
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
