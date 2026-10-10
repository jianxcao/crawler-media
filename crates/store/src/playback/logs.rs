use domain::{MediaId, UserId};
use rusqlite::{OptionalExtension, params};

use super::{PlayLogRow, media_id_from, user_id_from};
use super::{Store, StoreError};

impl Store {
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
                    end_position_ms, duration_ms, completed, device_id
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
        device_id: row.get(15)?,
    })
}
