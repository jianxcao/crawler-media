//! SQL aggregation for the playback watch-statistics report.
//!
//! Replaces the two full `list_logs(200_000, ..)` scans plus in-Rust grouping
//! with one windowed aggregation on the library connection. Visibility reuses
//! the TEMP visible-set pattern from `playback::log_scope`.

use std::collections::HashMap;

use domain::{MediaId, UserId};
use rusqlite::named_params;

use super::{Store, StoreError, media_id_from, user_id_from};

/// 单次统计允许的最大窗口天数与最大时区偏移（分钟）。
const MAX_DAYS: u16 = 365;
const MAX_TZ_MINUTES: i16 = 24 * 60;

const VISIBLE_CLAUSE: &str =
    " AND EXISTS (SELECT 1 FROM request_visible_media v WHERE v.id = l.media_id)";
const HIDDEN_CLAUSE: &str =
    " AND NOT EXISTS (SELECT 1 FROM request_visible_media v WHERE v.id = l.media_id)";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WatchTotals {
    pub plays: i64,
    pub watched_ms: i64,
    pub completed: i64,
    pub members: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WatchDay {
    pub day: i64,
    pub totals: WatchTotals,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchMember {
    pub user_id: UserId,
    pub totals: WatchTotals,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchClient {
    pub client: String,
    pub plays: i64,
    pub watched_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchMedia {
    pub media_id: MediaId,
    pub plays: i64,
    pub watched_ms: i64,
    pub members: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchWindow {
    pub totals: WatchTotals,
    pub days: Vec<WatchDay>,
    /// 7×24 的 watched_ms 矩阵：`hours[weekday][hour]`。
    pub hours: Vec<Vec<i64>>,
    pub users: Vec<WatchMember>,
    pub clients: Vec<WatchClient>,
    pub media: Vec<WatchMedia>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchReport {
    pub current: WatchWindow,
    pub previous: WatchWindow,
    /// 当前窗口内不可见 Media 的 distinct 数量。
    pub hidden_titles: usize,
}

/// `scoped` CTE：把时区偏移一次算好，day/hour 与 Rust 的
/// `div_euclid` / `rem_euclid` 语义一致。
///
/// SQL 的 `/` 对负数是向零截断，所以 `div_euclid(86400)` 必须写成 CASE：
/// 非负直接除，负值用 `-(((-x)+86399)/86400)` 取 floor。
/// `rem_euclid` 用 `((x % n) + n) % n`。
fn scoped_cte(visibility: &str) -> String {
    format!(
        "WITH request_visible_media AS (SELECT value AS id FROM json_each(:visible)), scoped AS (
            SELECT l.user_id, l.client, l.media_id, l.watched_ms, l.completed,
                   CASE WHEN l.started_at + :tz >= 0
                        THEN (l.started_at + :tz) / 86400
                        ELSE -(((-(l.started_at + :tz)) + 86399) / 86400) END AS day,
                   ((l.started_at + :tz) % 86400 + 86400) % 86400 / 3600 AS hour
            FROM playback_logs l
            WHERE l.started_at >= :start
              AND l.started_at < :end
              AND (:user IS NULL OR l.user_id = :user){visibility}
        )"
    )
}

impl Store {
    /// 当前窗口 + 上一窗口的播放统计（SQL 聚合，不把日志读进内存）。
    ///
    /// - `user`: `None` = 全部成员，`Some` = 只统计该成员。
    /// - `visible`: `None` = 不限制；`Some` = 只统计集合内的 Media（空集合 → 全隐藏）。
    /// - 当前窗口 `started_at >= now - days*86400`（无上界，保持既有语义）；
    ///   上一窗口 `[now - 2*days*86400, now - days*86400)`。
    pub fn playback_watch_report(
        &self,
        user: Option<UserId>,
        visible: Option<&[MediaId]>,
        now: i64,
        days: u16,
        tz_minutes: i16,
    ) -> Result<WatchReport, StoreError> {
        self.with_library_read(|db| db.playback_watch_report(user, visible, now, days, tz_minutes))
    }
}

#[path = "watch_aggregate/connection.rs"]
mod connection;
