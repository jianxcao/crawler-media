use domain::{MediaId, UserId};
use rusqlite::params;

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
        self.with_library_read(|db| db.list_logs(limit, before, before_id, days, now, user_id))
    }

    pub fn delete_logs(
        &self,
        user_id: UserId,
        media_ids: Option<&[MediaId]>,
        since_ms: Option<i64>,
    ) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.delete_logs(user_id, media_ids, since_ms))
    }

    pub fn delete_metrics(
        &self,
        user_id: UserId,
        media_ids: Option<&[MediaId]>,
        since_ms: Option<i64>,
    ) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.delete_metrics(user_id, media_ids, since_ms))
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
        self.with_library_write(|db| {
            db.insert_metric(
                user_id,
                media_id,
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
            )
        })
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

#[path = "logs/connection.rs"]
mod connection;
