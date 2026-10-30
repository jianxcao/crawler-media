//! Request-scoped playback history paging: composite keyset cursor plus a
//! visible-Media set resolved in SQL on the library connection, so callers
//! never load the whole `playback_logs` table to filter it in memory.

use domain::{MediaId, UserId};

use super::{PlayLogRow, Store, StoreError, media_id_from, user_id_from};

/// 单页上限（与 API 层一致）：越界报错，不静默 clamp。
const MAX_PAGE_LIMIT: usize = 200;

const LOG_COLS: &str = "l.id, l.user_id, l.media_id, l.season, l.episode, l.client,
       l.device_name, l.play_method, l.started_at, l.ended_at, l.watched_ms,
       l.start_position_ms, l.end_position_ms, l.duration_ms, l.completed, l.device_id";

/// 一次播放历史请求的可见范围与游标。
#[derive(Clone, Debug)]
pub struct PlaybackLogScope {
    /// 复合游标的时间分量：只保留 `started_at` 严格更早的行。
    pub before: Option<i64>,
    /// 复合游标的 id 分量；必须与 `before` 同时给出。
    pub before_id: Option<String>,
    /// 只保留 `started_at >= since` 的行（unix 秒）。
    pub since: Option<i64>,
    pub user_id: Option<UserId>,
    /// `None` = 不限制；`Some(vec![])` = 无任何可见 Media（结果必为空）。
    pub visible_media: Option<Vec<MediaId>>,
}

#[derive(Debug)]
pub struct PlaybackLogPage {
    pub entries: Vec<PlayLogRow>,
    pub has_more: bool,
    /// 同一 scope 内不可见 Media 的日志**行**数（不是 distinct Media）。
    pub hidden_count: usize,
}

impl Store {
    /// 按 scope 做 SQL keyset 分页；`hidden_count` 与 page 使用同一 scope 过滤。
    pub fn playback_log_page(
        &self,
        scope: &PlaybackLogScope,
        limit: usize,
    ) -> Result<PlaybackLogPage, StoreError> {
        self.with_library_read(|db| db.playback_log_page(scope, limit))
    }
}

/// `FROM ... WHERE ...`（不含可见性子句），page 与 hidden count 共用。
fn scope_filter(scope: &PlaybackLogScope) -> (String, Vec<rusqlite::types::Value>) {
    let mut sql = String::from(" FROM playback_logs l WHERE 1=1");
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(user_id) = &scope.user_id {
        sql.push_str(" AND l.user_id = ?");
        args.push(rusqlite::types::Value::Text(user_id.to_string()));
    }
    if let Some(since) = scope.since {
        sql.push_str(" AND l.started_at >= ?");
        args.push(rusqlite::types::Value::Integer(since));
    }
    if let (Some(before), Some(before_id)) = (scope.before, scope.before_id.as_deref()) {
        // 复合游标：跳过 started_at 更早的行，以及同一秒内 id 更小的行。
        sql.push_str(" AND (l.started_at < ? OR (l.started_at = ? AND l.id < ?))");
        args.push(rusqlite::types::Value::Integer(before));
        args.push(rusqlite::types::Value::Integer(before));
        args.push(rusqlite::types::Value::Text(before_id.to_string()));
    } else if let Some(before) = scope.before {
        sql.push_str(" AND l.started_at < ?");
        args.push(rusqlite::types::Value::Integer(before));
    }
    (sql, args)
}

/// `None` = 不限制（不加子句）；`Some`（含空集合）= 只保留 TEMP 表内的 Media。
fn visible_clause(visible: Option<&[MediaId]>) -> &'static str {
    if visible.is_some() {
        " AND EXISTS (SELECT 1 FROM request_visible_media v WHERE v.id = l.media_id)"
    } else {
        ""
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

#[path = "log_scope/connection.rs"]
mod connection;
