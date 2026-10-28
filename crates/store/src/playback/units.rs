use domain::{MediaId, UnitKey, UserId};
use rusqlite::{OptionalExtension, params};
use std::str::FromStr;

use super::unit_resume::{ResumeProgress, UnitIdentity};
use super::user_summary::SummaryScope;
use super::{Store, StoreError};
use super::{UnitRow, UnitState};

impl Store {
    pub fn file_owner_library(
        &self,
        file_id: domain::LedgerId,
    ) -> Result<Option<crate::Library>, StoreError> {
        let library_id: Option<String> = self
            .library
            .query_row(
                "SELECT library_id FROM library_files WHERE file_id = ?1",
                params![file_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(lib_id) = library_id else {
            return Ok(None);
        };
        self.get_library(&lib_id)
    }

    pub fn file_covers_unit(
        &self,
        file_id: domain::LedgerId,
        media_id: MediaId,
        unit: UnitKey,
    ) -> Result<bool, StoreError> {
        let exists: bool = self.library.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM library_file_units
                 WHERE file_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4
             )",
            params![
                file_id.to_string(),
                media_id.to_string(),
                unit.season,
                unit.episode
            ],
            |r| r.get(0),
        )?;
        Ok(exists)
    }

    pub fn unit_state(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
    ) -> Result<Option<UnitState>, StoreError> {
        self.library
            .query_row(
                "SELECT position_ms, played, favorite, duration_ms, audio_track,
                        subtitle_track, play_count, watch_activity_at, favorite_at,
                        CASE WHEN has_played_assertion = 1 THEN COALESCE(played_changed_at_ms, 0) END
                 FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                params![user_id.to_string(), media_id.to_string(), season, episode],
                |row| {
                    Ok(UnitState {
                        position_ms: row.get(0)?,
                        played: row.get::<_, i64>(1)? != 0,
                        favorite: row.get::<_, i64>(2)? != 0,
                        duration_ms: row.get(3)?,
                        audio_track: row.get(4)?,
                        subtitle_track: row.get(5)?,
                        play_count: row.get(6)?,
                        updated_at: row.get(7)?,
                        favorite_at: row.get(8)?,
                        played_assertion_at_ms: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_unit(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        duration_ms: Option<i64>,
        audio_track: Option<&str>,
        subtitle_track: Option<&str>,
        increment_play_count: bool,
        now: i64,
    ) -> Result<UnitState, StoreError> {
        let source = self.best_source_for_unit(
            &self.authorized_library_ids(user_id)?,
            media_id,
            UnitKey::new(season, episode),
            None,
        )?;
        // A played/favourite mark is a logical fact and must not require a file. A write that
        // claims resume state (position, duration, tracks) needs a bound source; without one the
        // claim is dropped instead of persisting a position that names no physical file.
        let (source, position_ms, duration_ms, audio_track, subtitle_track) = match source {
            Some(source) => (
                Some(domain::PhysicalSourceId {
                    file_id: source.file.file_id,
                    revision: source.file.version.revision,
                }),
                position_ms,
                duration_ms,
                audio_track,
                subtitle_track,
            ),
            None => (None, 0, None, None, None),
        };
        self.upsert_unit_bound(
            user_id,
            media_id,
            UnitKey::new(season, episode),
            source,
            position_ms,
            played,
            favorite,
            duration_ms,
            audio_track,
            subtitle_track,
            increment_play_count,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_unit_for_source(
        &self,
        user_id: UserId,
        media_id: MediaId,
        unit: UnitKey,
        source: domain::PhysicalSourceId,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        duration_ms: Option<i64>,
        audio_track: Option<&str>,
        subtitle_track: Option<&str>,
        increment_play_count: bool,
        now: i64,
    ) -> Result<UnitState, StoreError> {
        self.upsert_unit_bound(
            user_id,
            media_id,
            unit,
            Some(source),
            position_ms,
            played,
            favorite,
            duration_ms,
            audio_track,
            subtitle_track,
            increment_play_count,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_unit_bound(
        &self,
        user_id: UserId,
        media_id: MediaId,
        unit: UnitKey,
        source: Option<domain::PhysicalSourceId>,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        duration_ms: Option<i64>,
        audio_track: Option<&str>,
        subtitle_track: Option<&str>,
        increment_play_count: bool,
        now: i64,
    ) -> Result<UnitState, StoreError> {
        let (season, episode) = (unit.season, unit.episode);
        self.playback_write(|store| {
            crate::library_write::publish::ensure_media_writable(&store.library, media_id)?;
            let state = store.upsert_unit_internal(
                user_id,
                media_id,
                season,
                episode,
                Some(position_ms),
                played,
                favorite,
                duration_ms,
                audio_track,
                subtitle_track,
                increment_play_count,
                now,
                source,
            )?;
            store.refresh_user_library_summary(
                user_id,
                media_id,
                SummaryScope::for_unit(season, episode),
            )?;
            Ok(state)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_unit_internal(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        position_input: Option<i64>,
        played: Option<bool>,
        favorite: Option<bool>,
        duration_ms: Option<i64>,
        audio_track: Option<&str>,
        subtitle_track: Option<&str>,
        increment_play_count: bool,
        now: i64,
        source: Option<domain::PhysicalSourceId>,
    ) -> Result<UnitState, StoreError> {
        let played_input = played;
        let existing = self.unit_state(user_id, media_id, season, episode)?;
        let played = played_input.unwrap_or(existing.as_ref().map(|u| u.played).unwrap_or(false));
        let favorite_at = match favorite {
            Some(true) => now,
            Some(false) => 0,
            None => existing.as_ref().map(|unit| unit.favorite_at).unwrap_or(0),
        };
        let favorite = favorite.unwrap_or(existing.as_ref().map(|u| u.favorite).unwrap_or(false));
        let duration_ms = duration_ms.or(existing.as_ref().and_then(|u| u.duration_ms));
        let play_count = existing.as_ref().map(|u| u.play_count).unwrap_or(0)
            + if increment_play_count { 1 } else { 0 };
        let audio_track = audio_track
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|u| u.audio_track.clone()));
        let subtitle_track = subtitle_track
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|u| u.subtitle_track.clone()));

        let identity = UnitIdentity {
            user: user_id,
            media: media_id,
            season,
            episode,
        };
        let (has_played_assertion, played_changed_at) =
            self.played_assertion(&identity, played_input, now)?;
        let resume = self.resume_for_unit(
            &identity,
            position_input.map(|position| ResumeProgress {
                position,
                duration: duration_ms,
                audio: audio_track,
                subtitle: subtitle_track,
            }),
            source,
        )?;

        self.library.execute(
            "INSERT INTO playback_units
                (user_id, media_id, season, episode, position_ms, played, favorite,
                 duration_ms, audio_track, subtitle_track, play_count, watch_activity_at, favorite_at,
                 has_played_assertion, played_changed_at_ms, resume_file_id, resume_file_revision,
                 resume_source_version, timeline_mode, tracks_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
             ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
                position_ms = excluded.position_ms,
                played = excluded.played,
                favorite = excluded.favorite,
                duration_ms = excluded.duration_ms,
                audio_track = excluded.audio_track,
                subtitle_track = excluded.subtitle_track,
                play_count = excluded.play_count,
                watch_activity_at = excluded.watch_activity_at,
                favorite_at = excluded.favorite_at,
                has_played_assertion = excluded.has_played_assertion,
                played_changed_at_ms = excluded.played_changed_at_ms,
                resume_file_id = excluded.resume_file_id,
                resume_file_revision = excluded.resume_file_revision,
                resume_source_version = excluded.resume_source_version,
                timeline_mode = excluded.timeline_mode,
                tracks_version = excluded.tracks_version",
            params![
                user_id.to_string(),
                media_id.to_string(),
                season,
                episode,
                resume.position,
                if played { 1 } else { 0 },
                if favorite { 1 } else { 0 },
                resume.duration,
                resume.audio,
                resume.subtitle,
                play_count,
                now,
                favorite_at,
                has_played_assertion,
                played_changed_at,
                resume.file_id,
                resume.revision,
                resume.source_version,
                resume.timeline,
                resume.tracks_version,
            ],
        )?;
        self.unit_state(user_id, media_id, season, episode)?
            .ok_or_else(|| StoreError::Missing("unit just written".into()))
    }

    /// All distinct media ids and their most recent activity timestamp for a user.
    pub fn user_active_media_ids(
        &self,
        user_id: UserId,
    ) -> Result<Vec<(MediaId, i64)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT media_id, MAX(watch_activity_at) AS last_act
             FROM playback_units
             WHERE user_id = ?1 AND (position_ms > 0 OR played != 0 OR play_count > 0)
             GROUP BY media_id
             ORDER BY last_act DESC",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            let id_str: String = row.get(0)?;
            let updated: i64 = row.get(1)?;
            Ok((id_str, updated))
        })?;
        let mut result = Vec::new();
        for r in rows {
            let (id_str, updated) = r?;
            if let Ok(id) = MediaId::from_str(&id_str) {
                result.push((id, updated));
            }
        }
        Ok(result)
    }

    /// In-progress units (`position_ms > 0 AND played = 0`), newest first.
    ///
    /// This is the raw unit only. A newer season or show mark can still make
    /// the episode effectively played, so callers that page a resume list must
    /// apply that check before taking `limit` rows.
    pub fn user_resumable_units(
        &self,
        user_id: UserId,
    ) -> Result<Vec<(MediaId, i32, i32, i64, i64)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT media_id, season, episode, position_ms, watch_activity_at
             FROM playback_units
             WHERE user_id = ?1 AND position_ms > 0 AND played = 0
             ORDER BY watch_activity_at DESC",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            let media_str: String = row.get(0)?;
            let season: i32 = row.get(1)?;
            let episode: i32 = row.get(2)?;
            let pos: i64 = row.get(3)?;
            let updated: i64 = row.get(4)?;
            Ok((media_str, season, episode, pos, updated))
        })?;
        let mut list = Vec::new();
        for r in rows {
            let (m_str, season, episode, pos, updated) = r?;
            if let Ok(m_id) = MediaId::from_str(&m_str) {
                list.push((m_id, season, episode, pos, updated));
            }
        }
        Ok(list)
    }

    /// Favourited units ordered by favourite activity, newest first.
    /// `favorite_at = 0` is an unknown historical time and sorts last; readers
    /// must not substitute the still-changing playback clock.
    pub fn favorite_rows(
        &self,
        user_id: UserId,
    ) -> Result<Vec<(MediaId, i64, i32, i32)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT media_id, season, episode, favorite_at
             FROM playback_units
             WHERE user_id = ?1 AND favorite != 0
             ORDER BY favorite_at DESC, media_id, season, episode",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })?;
        let mut list = Vec::new();
        for row in rows {
            let (media, season, episode, favorited) = row?;
            if let Ok(media_id) = MediaId::from_str(&media) {
                list.push((media_id, favorited, season, episode));
            }
        }
        Ok(list)
    }

    /// All unit rows of a media (for marks aggregation / cascade).
    pub fn unit_rows(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Vec<UnitRow>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT season, episode, played, favorite, watch_activity_at, favorite_at
             FROM playback_units WHERE user_id = ?1 AND media_id = ?2",
        )?;
        let rows = stmt.query_map(params![user_id.to_string(), media_id.to_string()], |row| {
            Ok(UnitRow {
                season: row.get(0)?,
                episode: row.get(1)?,
                played: row.get::<_, i64>(2)? != 0,
                favorite: row.get::<_, i64>(3)? != 0,
                updated_at: row.get(4)?,
                favorite_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Clear viewing facts, retaining favorites, duration and track preferences.
    pub fn clear_units(
        &self,
        user_id: UserId,
        media_ids: Option<&[MediaId]>,
        since_ms: Option<i64>,
    ) -> Result<usize, StoreError> {
        if media_ids.is_some_and(|ids| ids.is_empty()) {
            return Ok(0);
        }
        self.playback_write(|store| {
            // Resolve the affected media before the update resets the activity clock.
            let affected = match media_ids {
                Some(ids) => ids.to_vec(),
                None => store.user_playback_media_ids(user_id, since_ms)?,
            };
            for media in &affected {
                crate::library_write::publish::ensure_media_writable(&store.library, *media)?;
            }
            // Clearing history drops the whole resume package and the played assertion; the
            // favourite is the user's library decision and survives it.
            let mut sql = String::from(
                "UPDATE playback_units SET
                    position_ms = 0, played = 0, has_played_assertion = 0,
                    played_changed_at_ms = NULL, play_count = 0, watch_activity_at = 0,
                    duration_ms = NULL, resume_file_id = NULL, resume_file_revision = NULL,
                    resume_source_version = NULL, timeline_mode = NULL, tracks_version = NULL,
                    audio_track = NULL, subtitle_track = NULL
                 WHERE user_id = ?",
            );
            let mut values: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Text(user_id.to_string())];
            if let Some(since) = since_ms {
                sql.push_str(" AND watch_activity_at >= ?");
                values.push(rusqlite::types::Value::Integer(since));
            }
            if let Some(ids) = media_ids {
                let marks = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                sql.push_str(&format!(" AND media_id IN ({marks})"));
                values.extend(
                    ids.iter()
                        .map(|id| rusqlite::types::Value::Text(id.to_string())),
                );
            }
            let changed = store
                .library
                .execute(&sql, rusqlite::params_from_iter(values))?;
            for media_id in affected {
                store.refresh_user_library_summary(user_id, media_id, SummaryScope::Whole)?;
            }
            Ok(changed)
        })
    }

    /// Media this user has playback facts for, optionally limited to activity
    /// at or after `since_ms`. One user's rows, never every user.
    fn user_playback_media_ids(
        &self,
        user_id: UserId,
        since_ms: Option<i64>,
    ) -> Result<Vec<MediaId>, StoreError> {
        let sql = if since_ms.is_some() {
            "SELECT DISTINCT media_id FROM playback_units
             WHERE user_id = ?1 AND watch_activity_at >= ?2"
        } else {
            "SELECT DISTINCT media_id FROM playback_units WHERE user_id = ?1"
        };
        let mut values: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(user_id.to_string())];
        if let Some(since) = since_ms {
            values.push(rusqlite::types::Value::Integer(since));
        }
        let mut statement = self.library.prepare(sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
            row.get::<_, String>(0)
        })?;
        let mut media = Vec::new();
        for row in rows {
            if let Ok(id) = row?.parse() {
                media.push(id);
            }
        }
        Ok(media)
    }
}
