use domain::{MediaId, PhysicalSourceId, TypedItemId, UnitKey, UserId};
use rusqlite::{OptionalExtension, params};
use std::str::FromStr;

use super::{Store, StoreError};
use super::user_summary::SummaryScope;
use super::{UNIT_WHOLE, UnitRow, UnitState};

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
            params![file_id.to_string(), media_id.to_string(), unit.season, unit.episode],
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
                        subtitle_track, play_count, watch_activity_at, favorite_at
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
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_unit_favorite(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        favorite: bool,
        now: i64,
    ) -> Result<(), StoreError> {
        self.playback_write(|store| {
            // canonical whole unit (-1, -1)
            store.set_unit_favorite_inner(user_id, media_id, season, episode, favorite, now)?;
            store.refresh_user_library_summary(
                user_id,
                media_id,
                SummaryScope::for_unit(season, episode),
            )
        })
    }

    fn set_unit_favorite_inner(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        favorite: bool,
        now: i64,
    ) -> Result<(), StoreError> {
        let existing = self.unit_state(user_id, media_id, season, episode)?;
        let favorite_at = match existing {
            Some(unit) if favorite && unit.favorite && unit.favorite_at != 0 => unit.favorite_at,
            _ => {
                if favorite {
                    now
                } else {
                    0
                }
            }
        };
        // A missing unit is favourite-only: the UPDATE below leaves every other
        // fact absent rather than materialising an inherited played mark.
        let changed = self.library.execute(
            "UPDATE playback_units SET favorite = ?5, favorite_at = ?6
             WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
            params![
                user_id.to_string(),
                media_id.to_string(),
                season,
                episode,
                favorite as i64,
                favorite_at,
            ],
        )?;
        if changed == 0 {
            self.library.execute(
                "INSERT INTO playback_units
                    (user_id, media_id, season, episode, position_ms, played, favorite,
                     duration_ms, audio_track, subtitle_track, play_count, watch_activity_at, favorite_at)
                 VALUES (?1, ?2, ?3, ?4, 0, 0, ?5, NULL, NULL, NULL, 0, 0, ?6)",
                params![
                    user_id.to_string(),
                    media_id.to_string(),
                    season,
                    episode,
                    favorite as i64,
                    favorite_at,
                ],
            )?;
        }
        Ok(())
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
        let source = self.best_source_for_unit(&self.authorized_library_ids(user_id)?, media_id,
            UnitKey::new(season, episode), None)?;
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
        self.upsert_unit_bound(user_id, media_id, UnitKey::new(season, episode), source, position_ms,
            played, favorite, duration_ms, audio_track, subtitle_track, increment_play_count, now)
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
        self.upsert_unit_bound(user_id, media_id, unit, Some(source), position_ms, played, favorite,
            duration_ms, audio_track, subtitle_track, increment_play_count, now)
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
            let state = store.upsert_unit_internal(
                user_id, media_id, season, episode, Some(position_ms), played, favorite,
                duration_ms, audio_track, subtitle_track, increment_play_count, now, source,
            )?;
            store.refresh_user_library_summary(user_id, media_id, SummaryScope::for_unit(season, episode))?;
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
        let position_ms = position_input.unwrap_or(0);
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

        // R04-A: Retain original Option<bool> for explicit played assertions.
        let (has_played_assertion, played_changed_at) = match played_input {
            Some(_) => (1, Some(now)),
            None => {
                let existing_assertion: Option<(i64, Option<i64>)> = self.library
                    .query_row(
                        "SELECT has_played_assertion, played_changed_at_ms FROM playback_units
                         WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                        params![user_id.to_string(), media_id.to_string(), season, episode],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                match existing_assertion {
                    Some((h, t)) => (h, t),
                    None => (0, None),
                }
            }
        };

        // R04-B & R04-C: Check existing resume package first before overwriting
        let existing_resume: Option<(Option<String>, Option<i64>, Option<String>, Option<String>, Option<i64>, i64, Option<i64>, Option<String>, Option<String>)> = self.library
            .query_row(
                "SELECT resume_file_id, resume_file_revision, resume_source_version, timeline_mode, tracks_version,
                        position_ms, duration_ms, audio_track, subtitle_track
                 FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                params![user_id.to_string(), media_id.to_string(), season, episode],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
            )
            .optional()?;

        let is_mark_only = position_input.is_none();
        let (bound_file_id, bound_rev, bound_src_ver, bound_timeline, bound_tracks_ver, pos_val, dur_val, aud_val, sub_val) =
            if is_mark_only {
                match existing_resume {
                    Some((Some(fid), Some(rev), Some(sver), Some(tmode), tracks_ver, p, d, a, s)) =>
                        (Some(fid), Some(rev), Some(sver), Some(tmode), tracks_ver, p, d, a, s),
                    _ => (None, None, None, None, None, 0, None, None, None),
                }
            } else {
                // Every real playback write is a complete resume package for the
                // currently preferred physical source, not an update to a stale binding.
                let file_binding: Option<(String, i64, String, i64)> = self.library
                    .query_row(
                        "SELECT f.file_id, f.file_revision, f.source_version, COALESCE(d.tracks_version, f.tracks_version)
                         FROM library_files f
                         JOIN library_file_units u ON f.file_id = u.file_id
                         LEFT JOIN file_details d ON d.file_id = f.file_id
                         WHERE u.media_id = ?1 AND u.season = ?2 AND u.episode = ?3
                           AND f.presence_state = 'present' AND f.publication_state = 'published'
                           AND (?4 IS NULL OR f.file_id = ?4) AND (?5 IS NULL OR f.file_revision = ?5)
                         ORDER BY f.file_revision DESC, f.file_id ASC LIMIT 1",
                        params![media_id.to_string(), season, episode,
                            source.as_ref().map(|selected| selected.file_id.to_string()),
                            source.as_ref().map(|selected| selected.revision)],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                match file_binding {
                    Some((fid, rev, sver, tver)) => (
                        Some(fid), Some(rev), Some(sver), Some("full_file".into()), Some(tver),
                        position_ms, duration_ms, audio_track.clone(), subtitle_track.clone(),
                    ),
                    None => (None, None, None, None, None, 0, None, None, None),
                }
            };

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
                pos_val,
                if played { 1 } else { 0 },
                if favorite { 1 } else { 0 },
                dur_val,
                aud_val,
                sub_val,
                play_count,
                now,
                favorite_at,
                has_played_assertion,
                played_changed_at,
                bound_file_id,
                bound_rev,
                bound_src_ver,
                bound_timeline,
                bound_tracks_ver,
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
    /// apply that check before taking `limit` rows. A copied non-TV `(0, 0)`
    /// row is retired with its canonical copy, so it cannot take a second slot.
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
    pub fn favorite_rows(&self, user_id: UserId) -> Result<Vec<(MediaId, i64, i32, i32)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT media_id, season, episode, favorite_at
             FROM playback_units
             WHERE user_id = ?1 AND favorite != 0
             ORDER BY favorite_at DESC, media_id, season, episode",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
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

    /// Set played marks with cascade (whole/season marks every unit below);
    /// favorite is stored on exactly the target unit (favorite level = the
    /// most recent favorited unit).
    #[allow(clippy::too_many_arguments)]
    pub fn set_unit_marks(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        played: Option<bool>,
        favorite: Option<bool>,
        now: i64,
    ) -> Result<(), StoreError> {
        self.playback_write(|store| {
            // canonical whole unit (-1, -1)
            store.set_unit_marks_inner(user_id, media_id, season, episode, played, favorite, now)?;
            store.refresh_user_library_summary(
                user_id,
                media_id,
                SummaryScope::for_unit(season, episode),
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn set_unit_marks_inner(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        played: Option<bool>,
        favorite: Option<bool>,
        now: i64,
    ) -> Result<(), StoreError> {
        if let Some(favorite) = favorite {
            // Favourite activity is its own clock. Playback fields stay on the
            // row that already owns them; a missing unit is favourite-only.
            if played.is_none() {
                self.set_unit_favorite_inner(user_id, media_id, season, episode, favorite, now)?;
            } else {
                let existing = self.unit_state(user_id, media_id, season, episode)?;
                self.upsert_unit_internal(
                    user_id,
                    media_id,
                    season,
                    episode,
                    Some(existing.as_ref().map(|unit| unit.position_ms).unwrap_or(0)),
                    played,
                    Some(favorite),
                    existing.as_ref().and_then(|unit| unit.duration_ms),
                    existing.as_ref().and_then(|unit| unit.audio_track.as_deref()),
                    existing.as_ref().and_then(|unit| unit.subtitle_track.as_deref()),
                    false,
                    now,
                    None,
                )?;
            }
            if !favorite && season == UNIT_WHOLE && episode == UNIT_WHOLE {
                self.library.execute(
                    "UPDATE playback_units SET favorite = 0, favorite_at = 0
                     WHERE user_id = ?1 AND media_id = ?2",
                    params![user_id.to_string(), media_id.to_string()],
                )?;
            }
        }
        if let Some(played) = played {
            let (s_from, e_from) = match (season, episode) {
                (UNIT_WHOLE, _) => (UNIT_WHOLE, UNIT_WHOLE),
                (s, UNIT_WHOLE) => (s, UNIT_WHOLE),
                (s, e) => (s, e),
            };
            let mut stmt = self.library.prepare(
                "UPDATE playback_units
                 SET played = ?3,
                     has_played_assertion = 1,
                     played_changed_at_ms = ?4,
                     watch_activity_at = ?4
                 WHERE user_id = ?1 AND media_id = ?2
                   AND season >= ?5 AND episode >= ?6
                   AND (?5 = -1 OR season = ?5)
                   AND (?6 = -1 OR episode = ?6)",
            )?;
            let _ = stmt.execute(params![
                user_id.to_string(),
                media_id.to_string(),
                if played { 1 } else { 0 },
                now,
                s_from,
                e_from,
            ])?;
            drop(stmt);
            // R05-C: Ensure target unit exists so unplayed/played assertion is persisted even if row was absent
            let existing = self.unit_state(user_id, media_id, season, episode)?;
            if existing.is_none() {
                self.upsert_unit_internal(
                    user_id,
                    media_id,
                    season,
                    episode,
                    None, // Mark-only
                    Some(played),
                    favorite,
                    None,
                    None,
                    None,
                    false,
                    now,
                    None,
                )?;
            }
        }
        Ok(())
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

    pub fn delete_playback_for_media(&self, media_id: domain::MediaId) -> Result<(), StoreError> {
        self.playback_write(|store| {
            let id_str = media_id.to_string();
            store.library.execute(
                "DELETE FROM playback_units WHERE media_id = ?1",
                params![&id_str],
            )?;
            store.library.execute(
                "DELETE FROM playback_sessions WHERE media_id = ?1",
                params![&id_str],
            )?;
            store.library.execute(
                "DELETE FROM playback_logs WHERE media_id = ?1",
                params![&id_str],
            )?;
            store.library.execute(
                "DELETE FROM playback_metrics WHERE media_id = ?1",
                params![&id_str],
            )?;
            // Every user's facts for this media are gone, so their summaries are
            // empty by definition. Scoped by media, not by user.
            store.library.execute(
                "DELETE FROM user_library_items WHERE media_id = ?1",
                params![&id_str],
            )?;
            Ok(())
        })
    }
}
