//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub(super) fn stored_resume(
        &self,
        user: UserId,
        media_id: MediaId,
        unit: UnitKey,
    ) -> Result<Option<StoredResume>, StoreError> {
        self.library
            .query_row(
                "SELECT played, position_ms, duration_ms, audio_track, subtitle_track,
                        resume_file_id, resume_file_revision, resume_source_version, timeline_mode,
                        tracks_version
                 FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                params![
                    user.to_string(),
                    media_id.to_string(),
                    unit.season,
                    unit.episode
                ],
                |row| {
                    let binding = match (
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ) {
                        (
                            Some(file_id),
                            Some(revision),
                            Some(source_version),
                            Some(timeline_mode),
                            Some(tracks_version),
                        ) => Some(StoredBinding {
                            file_id,
                            revision,
                            source_version,
                            timeline_mode,
                            tracks_version,
                        }),
                        _ => None,
                    };
                    Ok(StoredResume {
                        played: row.get::<_, i64>(0)? != 0,
                        position_ms: row.get(1)?,
                        duration_ms: row.get(2)?,
                        audio_track: row.get(3)?,
                        subtitle_track: row.get(4)?,
                        binding,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn mutate_playback(
        &self,
        media: MediaId,
        change: &PlaybackMutation,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if change.played.is_none() && change.favorite.is_none() {
            return Ok(());
        }
        let tx = crate::write_tx(self.library)?;
        crate::library_write::publish::ensure_media_writable(&tx, media)?;
        let (season, episode) = match change.scope {
            UnitScope::Whole => (-1, -1),
            UnitScope::Season(s) => (s as i32, -1),
            UnitScope::Episode { season, episode } => (season as i32, episode as i32),
        };
        let user_id = change.user_id.to_string();
        let media_id = media.to_string();
        let played = change.played.map(i64::from);
        let favorite = change.favorite.map(i64::from);
        tx.execute(
            "INSERT INTO playback_units
                (user_id, media_id, season, episode, played, has_played_assertion,
                 played_changed_at_ms, favorite, favorite_at, watch_activity_at)
             VALUES
                (?1, ?2, ?3, ?4, COALESCE(?5, 0), ?5 IS NOT NULL,
                 CASE WHEN ?5 IS NOT NULL THEN ?6 END, COALESCE(?7, 0),
                 CASE WHEN ?7 = 1 THEN ?8 ELSE 0 END, 0)
             ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
                played = COALESCE(?5, played),
                has_played_assertion = CASE WHEN ?5 IS NOT NULL THEN 1 ELSE has_played_assertion END,
                played_changed_at_ms = CASE WHEN ?5 IS NOT NULL THEN ?6 ELSE played_changed_at_ms END,
                favorite = COALESCE(?7, favorite),
                favorite_at = CASE
                    WHEN ?7 = 1 AND favorite = 1 AND favorite_at != 0 THEN favorite_at
                    WHEN ?7 = 1 THEN ?8
                    WHEN ?7 = 0 THEN 0
                    ELSE favorite_at END",
            params![
                user_id,
                media_id,
                season,
                episode,
                played,
                now_ms,
                favorite,
                now_ms / 1000
            ],
        )?;
        if change.favorite == Some(false) && change.scope == UnitScope::Whole {
            tx.execute(
                "UPDATE playback_units SET favorite = 0, favorite_at = 0
                 WHERE user_id = ?1 AND media_id = ?2 AND favorite != 0",
                params![user_id, media_id],
            )?;
        }
        self.refresh_user_library_summary(change.user_id, media)?;
        tx.commit()?;
        Ok(())
    }

    pub fn begin_media_merge(
        &self,
        request: &super::super::types::MediaMergeRequest,
        now_ms: i64,
    ) -> Result<String, StoreError> {
        self.begin_merge(request, now_ms)
    }
}
