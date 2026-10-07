//! Playback activity store: per-unit progress/marks, live sessions, play
//! logs, and quality metrics. Units use (-1, -1) as the whole-media sentinel
//! (movies / series-level resume), matching the frontend's PlaybackUnit.

use std::str::FromStr;

use domain::{MediaId, UserId};
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

pub const UNIT_WHOLE: i32 = -1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitState {
    pub position_ms: i64,
    pub played: bool,
    pub favorite: bool,
    pub duration_ms: Option<i64>,
    pub audio_track: Option<String>,
    pub subtitle_track: Option<String>,
    pub play_count: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitRow {
    pub season: i32,
    pub episode: i32,
    pub played: bool,
    pub favorite: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub device_id: String,
    pub user_id: UserId,
    pub media_id: MediaId,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub client: Option<String>,
    pub device_name: Option<String>,
    pub client_version: Option<String>,
    pub play_method: String,
    pub position_ms: i64,
    pub start_position_ms: i64,
    pub duration_ms: Option<i64>,
    pub paused: bool,
    pub watched_ms: i64,
    pub rate_bps: i64,
    pub bytes_sent: i64,
    pub connections: i32,
    pub admin_ended: bool,
    pub started_at: i64,
    pub last_report_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayLogRow {
    pub id: String,
    pub user_id: UserId,
    pub media_id: MediaId,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub client: Option<String>,
    pub device_name: Option<String>,
    pub play_method: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub watched_ms: i64,
    pub start_position_ms: i64,
    pub end_position_ms: i64,
    pub duration_ms: Option<i64>,
    pub completed: bool,
}

fn media_id_from(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<MediaId> {
    MediaId::from_str(&row.get::<_, String>(index)?).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn user_id_from(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<UserId> {
    UserId::from_str(&row.get::<_, String>(index)?).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

impl Store {
    // ------------------------------------------------------------------
    // Units (per-unit progress + marks)
    // ------------------------------------------------------------------

    pub fn unit_state(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
    ) -> Result<Option<UnitState>, StoreError> {
        self.subscribe
            .query_row(
                "SELECT position_ms, played, favorite, duration_ms, audio_track,
                        subtitle_track, play_count, updated_at
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
        let existing = self.unit_state(user_id, media_id, season, episode)?;
        let played = played.unwrap_or(existing.as_ref().map(|u| u.played).unwrap_or(false));
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
        self.subscribe.execute(
            "INSERT INTO playback_units
                (user_id, media_id, season, episode, position_ms, played, favorite,
                 duration_ms, audio_track, subtitle_track, play_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
                position_ms = excluded.position_ms,
                played = excluded.played,
                favorite = excluded.favorite,
                duration_ms = excluded.duration_ms,
                audio_track = excluded.audio_track,
                subtitle_track = excluded.subtitle_track,
                play_count = excluded.play_count,
                updated_at = excluded.updated_at",
            params![
                user_id.to_string(),
                media_id.to_string(),
                season,
                episode,
                position_ms,
                played as i64,
                favorite as i64,
                duration_ms,
                audio_track,
                subtitle_track,
                play_count,
                now,
            ],
        )?;
        Ok(self
            .unit_state(user_id, media_id, season, episode)?
            .expect("unit just written"))
    }

    /// Copy a legacy `(0, 0)` watch unit onto the canonical `(-1, -1)` unit for
    /// whole-file media; TV keeps `(0, 0)` as a real season/episode pair.
    pub fn copy_legacy_movie_unit_if_missing(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<(), StoreError> {
        match self.get_media(media_id)? {
            Some(media) if media.kind != domain::MediaKind::Tv => {}
            _ => return Ok(()),
        }
        self.subscribe.execute(
            "INSERT OR IGNORE INTO playback_units
                (user_id, media_id, season, episode, position_ms, played, favorite,
                 duration_ms, audio_track, subtitle_track, play_count, updated_at)
             SELECT user_id, media_id, -1, -1, position_ms, played, favorite,
                    duration_ms, audio_track, subtitle_track, play_count, updated_at
             FROM playback_units
             WHERE user_id = ?1 AND media_id = ?2 AND season = 0 AND episode = 0",
            params![user_id.to_string(), media_id.to_string()],
        )?;
        Ok(())
    }

    /// All distinct media ids and their most recent activity timestamp for a user.
    pub fn user_active_media_ids(
        &self,
        user_id: UserId,
    ) -> Result<Vec<(MediaId, i64)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT media_id, MAX(updated_at) AS last_act
             FROM playback_units
             WHERE user_id = ?1
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

    /// All unit rows of a media (for marks aggregation / cascade).
    pub fn unit_rows(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Vec<UnitRow>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT season, episode, played, favorite, updated_at
             FROM playback_units WHERE user_id = ?1 AND media_id = ?2",
        )?;
        let rows = stmt.query_map(params![user_id.to_string(), media_id.to_string()], |row| {
            Ok(UnitRow {
                season: row.get(0)?,
                episode: row.get(1)?,
                played: row.get::<_, i64>(2)? != 0,
                favorite: row.get::<_, i64>(3)? != 0,
                updated_at: row.get(4)?,
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
        if let Some(favorite) = favorite {
            let existing = self.unit_state(user_id, media_id, season, episode)?;
            let played = played
                .or(existing.as_ref().map(|u| u.played))
                .unwrap_or(false);
            self.upsert_unit(
                user_id,
                media_id,
                season,
                episode,
                existing.as_ref().map(|u| u.position_ms).unwrap_or(0),
                Some(played),
                Some(favorite),
                existing.as_ref().and_then(|u| u.duration_ms),
                existing.as_ref().and_then(|u| u.audio_track.as_deref()),
                existing.as_ref().and_then(|u| u.subtitle_track.as_deref()),
                false,
                now,
            )?;
            // 当取消整剧收藏（season=-1, episode=-1）时，级联将名下所有的单集/单季收藏一并置为 false，
            // 避免出现整剧已取消收藏但列表中依然因为某集残留而被判定为已收藏。
            if !favorite && season == UNIT_WHOLE && episode == UNIT_WHOLE {
                let _ = self.subscribe.execute(
                    "UPDATE playback_units SET favorite = 0, updated_at = ?3
                     WHERE user_id = ?1 AND media_id = ?2",
                    params![user_id.to_string(), media_id.to_string(), now],
                );
            }
        }
        if let Some(played) = played {
            let (s_from, e_from) = match (season, episode) {
                (UNIT_WHOLE, _) => (UNIT_WHOLE, UNIT_WHOLE),
                (s, UNIT_WHOLE) => (s, UNIT_WHOLE),
                (s, e) => (s, e),
            };
            let mut stmt = self.subscribe.prepare(
                "UPDATE playback_units SET played = ?3, updated_at = ?4
                 WHERE user_id = ?1 AND media_id = ?2
                   AND season >= ?5 AND episode >= ?6
                   AND (?5 = -1 OR season = ?5)
                   AND (?6 = -1 OR episode = ?6)",
            )?;
            let _ = stmt.execute(params![
                user_id.to_string(),
                media_id.to_string(),
                played as i64,
                now,
                s_from,
                e_from,
            ])?;
            drop(stmt);
            // Ensure the target unit exists so unplayed_count is well-defined.
            if played {
                let existing = self.unit_state(user_id, media_id, season, episode)?;
                if existing.is_none() {
                    self.upsert_unit(
                        user_id,
                        media_id,
                        season,
                        episode,
                        0,
                        Some(true),
                        favorite,
                        None,
                        None,
                        None,
                        false,
                        now,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Remove units for the user within an optional media scope / since window.
    pub fn clear_units(
        &self,
        user_id: UserId,
        media_ids: Option<&[MediaId]>,
        since_ms: Option<i64>,
    ) -> Result<usize, StoreError> {
        let mut sql = String::from("DELETE FROM playback_units WHERE user_id = ?");
        let mut values: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(user_id.to_string())];
        if let Some(since) = since_ms {
            sql.push_str(" AND updated_at >= ?");
            values.push(rusqlite::types::Value::Integer(since));
        }
        if let Some(ids) = media_ids {
            if ids.is_empty() {
                return Ok(0);
            }
            let marks = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            sql.push_str(&format!(" AND media_id IN ({marks})"));
            values.extend(
                ids.iter()
                    .map(|id| rusqlite::types::Value::Text(id.to_string())),
            );
        }
        let changed = self
            .subscribe
            .execute(&sql, rusqlite::params_from_iter(values))?;
        Ok(changed)
    }

    pub fn delete_playback_for_media(&self, media_id: domain::MediaId) -> Result<(), StoreError> {
        let id_str = media_id.to_string();
        self.subscribe.execute(
            "DELETE FROM playback_units WHERE media_id = ?1",
            params![&id_str],
        )?;
        self.subscribe.execute(
            "DELETE FROM playback_sessions WHERE media_id = ?1",
            params![&id_str],
        )?;
        self.subscribe.execute(
            "DELETE FROM playback_logs WHERE media_id = ?1",
            params![&id_str],
        )?;
        self.subscribe.execute(
            "DELETE FROM playback_metrics WHERE media_id = ?1",
            params![&id_str],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Sessions
    // ------------------------------------------------------------------

    pub fn upsert_session(&self, session: &SessionRow) -> Result<(), StoreError> {
        self.subscribe.execute(
            "INSERT OR REPLACE INTO playback_sessions
                (device_id, user_id, media_id, season, episode, client, device_name,
                 client_version, play_method, position_ms, start_position_ms, duration_ms,
                 paused, watched_ms, rate_bps, bytes_sent, connections, admin_ended,
                 started_at, last_report_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20)",
            params![
                session.device_id,
                session.user_id.to_string(),
                session.media_id.to_string(),
                session.season,
                session.episode,
                session.client,
                session.device_name,
                session.client_version,
                session.play_method,
                session.position_ms,
                session.start_position_ms,
                session.duration_ms,
                session.paused as i64,
                session.watched_ms,
                session.rate_bps,
                session.bytes_sent,
                session.connections,
                session.admin_ended as i64,
                session.started_at,
                session.last_report_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_session(
        &self,
        user_id: UserId,
        device_id: &str,
    ) -> Result<Option<SessionRow>, StoreError> {
        self.subscribe
            .query_row(
                "SELECT device_id, user_id, media_id, season, episode, client, device_name,
                        client_version, play_method, position_ms, start_position_ms,
                        duration_ms, paused, watched_ms, rate_bps, bytes_sent, connections,
                        admin_ended, started_at, last_report_at
                 FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
                params![user_id.to_string(), device_id],
                map_session,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn sessions_for_device(&self, device_id: &str) -> Result<Vec<SessionRow>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT device_id, user_id, media_id, season, episode, client, device_name,
                    client_version, play_method, position_ms, start_position_ms,
                    duration_ms, paused, watched_ms, rate_bps, bytes_sent, connections,
                    admin_ended, started_at, last_report_at
             FROM playback_sessions WHERE device_id = ?1
             ORDER BY last_report_at DESC",
        )?;
        let rows = stmt.query_map(params![device_id], map_session)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Sessions with a heartbeat inside the timeout window.
    pub fn active_sessions(
        &self,
        now: i64,
        timeout_secs: i64,
    ) -> Result<Vec<SessionRow>, StoreError> {
        let cutoff = now - timeout_secs;
        let mut stmt = self.subscribe.prepare(
            "SELECT device_id, user_id, media_id, season, episode, client, device_name,
                    client_version, play_method, position_ms, start_position_ms,
                    duration_ms, paused, watched_ms, rate_bps, bytes_sent, connections,
                    admin_ended, started_at, last_report_at
             FROM playback_sessions WHERE last_report_at >= ?1
             ORDER BY last_report_at DESC",
        )?;
        let rows = stmt.query_map(params![cutoff], map_session)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Close a session into a play log; returns the log row. Missing session = None.
    pub fn close_session(
        &self,
        user_id: UserId,
        device_id: &str,
        now: i64,
    ) -> Result<Option<PlayLogRow>, StoreError> {
        let Some(session) = self.get_session(user_id, device_id)? else {
            return Ok(None);
        };
        self.subscribe.execute(
            "DELETE FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
            params![user_id.to_string(), device_id],
        )?;
        let completed = session
            .duration_ms
            .is_some_and(|duration| session.position_ms + 60_000 >= duration);
        let log = PlayLogRow {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: session.user_id,
            media_id: session.media_id,
            season: session.season,
            episode: session.episode,
            client: session.client,
            device_name: session.device_name,
            play_method: session.play_method,
            started_at: session.started_at,
            ended_at: now,
            watched_ms: session.watched_ms,
            start_position_ms: session.start_position_ms,
            end_position_ms: session.position_ms,
            duration_ms: session.duration_ms,
            completed,
        };
        self.subscribe.execute(
            "INSERT INTO playback_logs
                (id, user_id, media_id, season, episode, client, device_name, play_method,
                 started_at, ended_at, watched_ms, start_position_ms, end_position_ms,
                 duration_ms, completed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                log.id,
                log.user_id.to_string(),
                log.media_id.to_string(),
                log.season,
                log.episode,
                log.client,
                log.device_name,
                log.play_method,
                log.started_at,
                log.ended_at,
                log.watched_ms,
                log.start_position_ms,
                log.end_position_ms,
                log.duration_ms,
                log.completed as i64,
            ],
        )?;
        Ok(Some(log))
    }

    pub fn delete_session(&self, user_id: UserId, device_id: &str) -> Result<bool, StoreError> {
        let n = self.subscribe.execute(
            "DELETE FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
            params![user_id.to_string(), device_id],
        )?;
        Ok(n > 0)
    }

    pub fn end_device_session(
        &self,
        user_id: UserId,
        device_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        Ok(self.close_session(user_id, device_id, now)?.is_some())
    }

    /// Close every session whose heartbeat is older than the timeout.
    pub fn sweep_stale_sessions(&self, now: i64, timeout_secs: i64) -> Result<usize, StoreError> {
        let cutoff = now - timeout_secs;
        let session_keys: Vec<(UserId, String)> = {
            let mut stmt = self.subscribe.prepare(
                "SELECT user_id, device_id FROM playback_sessions WHERE last_report_at < ?1",
            )?;
            let rows = stmt.query_map(params![cutoff], |row| {
                Ok((user_id_from(row, 0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut closed = 0;
        for (user_id, device_id) in session_keys {
            if self.close_session(user_id, &device_id, now)?.is_some() {
                closed += 1;
            }
        }
        Ok(closed)
    }

    // ------------------------------------------------------------------
    // Logs & metrics
    // ------------------------------------------------------------------

    pub fn list_logs(
        &self,
        limit: usize,
        before: Option<i64>,
        before_id: Option<&str>,
        days: Option<i64>,
        now: i64,
        user_id: Option<UserId>,
    ) -> Result<Vec<PlayLogRow>, StoreError> {
        let mut sql = String::from(
            "SELECT id, user_id, media_id, season, episode, client, device_name,
                    play_method, started_at, ended_at, watched_ms, start_position_ms,
                    end_position_ms, duration_ms, completed
             FROM playback_logs WHERE 1=1",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let (Some(before), Some(bid)) = (before, before_id) {
            // Composite cursor: skip records at exactly `before` that have
            // id >= before_id, and all records strictly before `before`.
            sql.push_str(" AND (started_at < ? OR (started_at = ? AND id < ?))");
            args.push(rusqlite::types::Value::Integer(before));
            args.push(rusqlite::types::Value::Integer(before));
            args.push(rusqlite::types::Value::Text(bid.to_string()));
        } else if let Some(before) = before {
            sql.push_str(" AND started_at < ?");
            args.push(rusqlite::types::Value::Integer(before));
        }
        if let Some(days) = days {
            sql.push_str(" AND started_at >= ?");
            args.push(rusqlite::types::Value::Integer(now - days * 86_400));
        }
        if let Some(user_id) = user_id {
            sql.push_str(" AND user_id = ?");
            args.push(rusqlite::types::Value::Text(user_id.to_string()));
        }
        sql.push_str(" ORDER BY started_at DESC, id DESC LIMIT ?");
        args.push(rusqlite::types::Value::Integer(limit as i64));
        let mut stmt = self.subscribe.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), map_log)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// True when more logs exist before `before` (for cursor pagination).
    pub fn logs_have_more(
        &self,
        before: Option<i64>,
        days: Option<i64>,
        now: i64,
        user_id: Option<UserId>,
    ) -> Result<bool, StoreError> {
        let mut sql = String::from("SELECT 1 FROM playback_logs WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(before) = before {
            sql.push_str(" AND started_at < ?");
            args.push(rusqlite::types::Value::Integer(before));
        }
        if let Some(days) = days {
            sql.push_str(" AND started_at >= ?");
            args.push(rusqlite::types::Value::Integer(now - days * 86_400));
        }
        if let Some(user_id) = user_id {
            sql.push_str(" AND user_id = ?");
            args.push(rusqlite::types::Value::Text(user_id.to_string()));
        }
        sql.push_str(" LIMIT 1");
        let mut stmt = self.subscribe.prepare(&sql)?;
        Ok(stmt
            .query_row(rusqlite::params_from_iter(args), |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn delete_logs(
        &self,
        user_id: UserId,
        media_ids: Option<&[MediaId]>,
        since_ms: Option<i64>,
    ) -> Result<usize, StoreError> {
        let mut sql = String::from("DELETE FROM playback_logs WHERE user_id = ?");
        let mut values: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(user_id.to_string())];
        if let Some(since) = since_ms {
            sql.push_str(" AND started_at >= ?");
            values.push(rusqlite::types::Value::Integer(since));
        }
        if let Some(ids) = media_ids {
            if ids.is_empty() {
                return Ok(0);
            }
            let marks = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            sql.push_str(&format!(" AND media_id IN ({marks})"));
            values.extend(
                ids.iter()
                    .map(|id| rusqlite::types::Value::Text(id.to_string())),
            );
        }
        let changed = self
            .subscribe
            .execute(&sql, rusqlite::params_from_iter(values))?;
        Ok(changed)
    }

    pub fn delete_metrics(
        &self,
        user_id: UserId,
        media_ids: Option<&[MediaId]>,
        since_ms: Option<i64>,
    ) -> Result<usize, StoreError> {
        let mut sql = String::from("DELETE FROM playback_metrics WHERE user_id = ?");
        let mut values: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(user_id.to_string())];
        if let Some(since) = since_ms {
            sql.push_str(" AND recorded_at >= ?");
            values.push(rusqlite::types::Value::Integer(since));
        }
        if let Some(ids) = media_ids {
            if ids.is_empty() {
                return Ok(0);
            }
            let marks = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            sql.push_str(&format!(" AND media_id IN ({marks})"));
            values.extend(
                ids.iter()
                    .map(|id| rusqlite::types::Value::Text(id.to_string())),
            );
        }
        let changed = self
            .subscribe
            .execute(&sql, rusqlite::params_from_iter(values))?;
        Ok(changed)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_metric(
        &self,
        user_id: UserId,
        media_id: MediaId,
        tier: i32,
        engine: Option<&str>,
        ttff_ms: Option<i64>,
        rebuffer_ms: i64,
        rebuffer_count: i64,
        seek_count: i64,
        dropped_frames: Option<i64>,
        total_frames: Option<i64>,
        watched_ms: i64,
        now: i64,
    ) -> Result<(), StoreError> {
        self.subscribe.execute(
            "INSERT INTO playback_metrics
                (id, user_id, media_id, tier, engine, ttff_ms, rebuffer_ms,
                 rebuffer_count, seek_count, dropped_frames, total_frames,
                 watched_ms, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                uuid::Uuid::new_v4().to_string(),
                user_id.to_string(),
                media_id.to_string(),
                tier,
                engine,
                ttff_ms,
                rebuffer_ms,
                rebuffer_count,
                seek_count,
                dropped_frames,
                total_frames,
                watched_ms,
                now,
            ],
        )?;
        Ok(())
    }
}

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
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
    })
}

fn map_log(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlayLogRow> {
    Ok(PlayLogRow {
        id: row.get(0)?,
        user_id: user_id_from(row, 1)?,
        media_id: media_id_from(row, 2)?,
        season: row.get(3)?,
        episode: row.get(4)?,
        client: row.get(5)?,
        device_name: row.get(6)?,
        play_method: row.get(7)?,
        started_at: row.get(8)?,
        ended_at: row.get(9)?,
        watched_ms: row.get(10)?,
        start_position_ms: row.get(11)?,
        end_position_ms: row.get(12)?,
        duration_ms: row.get(13)?,
        completed: row.get::<_, i64>(14)? != 0,
    })
}
