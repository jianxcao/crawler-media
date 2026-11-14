use std::collections::HashMap;

use domain::SubscribeId;
use rusqlite::params;

use super::{Store, StoreError};

/// 工单（wanted）单元的搜索/投递历史。状态本身按 facts（imported）/
/// pending（grabbed）实时推导，这里只存会随时间变化的履历字段。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WantedHistory {
    pub search_attempts: i32,
    pub last_search_at: Option<i64>,
    pub grabbed_at: Option<i64>,
    pub imported_at: Option<i64>,
    pub grab_title: Option<String>,
    pub last_reject_reason: Option<String>,
}

pub(crate) fn wanted_key(season: Option<u32>, episode: Option<u32>) -> (i64, i64) {
    (season.map_or(-1, i64::from), episode.map_or(-1, i64::from))
}

impl Store {
    /// 批量 wanted 更新的事务 seam：在 subscribe 单连接上包一个事务，
    /// 让「24 集」只有一次提交、失败时整体回滚。调用方已经在事务里时
    /// （`!is_autocommit()`）直接执行——与 `playback_write` 同一约定，
    /// 不嵌套开事务。
    pub fn wanted_write<T>(
        &self,
        operation: impl FnOnce(&crate::SubscribeConnection<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_subscribe_write(|db| db.wanted_write(operation))
    }

    /// 一轮搜索后：对每个还在追踪的单元累加 search_attempts 并刷新 last_search_at。
    /// 已入库（imported）的单元不在轮次里，不动。
    pub fn touch_wanted_searches(
        &self,
        subscribe_id: SubscribeId,
        units: &[(Option<u32>, Option<u32>)],
        now: i64,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.touch_wanted_searches(subscribe_id, units, now))
    }

    /// Clear retry cooldown fields only for the supplied still-missing units.
    pub fn reset_wanted_search_cooldowns(
        &self,
        subscribe_id: SubscribeId,
        units: &[(Option<u32>, Option<u32>)],
    ) -> Result<usize, StoreError> {
        self.with_subscribe_write(|db| db.reset_wanted_search_cooldowns(subscribe_id, units))
    }

    /// 候选被规则组拒绝：记录最近一次拒绝原因（站点 · 种子名）。
    pub fn record_wanted_reject(
        &self,
        subscribe_id: SubscribeId,
        season: Option<u32>,
        episode: Option<u32>,
        reason: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| {
            db.record_wanted_reject(subscribe_id, season, episode, reason, now)
        })
    }

    /// 一个种子被投给下载器：记录抓取时间与种子名。
    pub fn record_wanted_grab(
        &self,
        subscribe_id: SubscribeId,
        season: Option<u32>,
        episode: Option<u32>,
        title: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| {
            db.record_wanted_grab(subscribe_id, season, episode, title, now)
        })
    }

    /// 转移完成：该单元已入库。
    pub fn mark_wanted_imported(
        &self,
        subscribe_id: SubscribeId,
        season: Option<u32>,
        episode: Option<u32>,
        now: i64,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.mark_wanted_imported(subscribe_id, season, episode, now))
    }

    /// 全部工单历史（(season, episode) 键与 facts 同构，-1 表示 movie）。
    pub fn load_wanted_history(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<HashMap<(Option<u32>, Option<u32>), WantedHistory>, StoreError> {
        self.with_subscribe_read(|db| db.load_wanted_history(subscribe_id))
    }
}

#[path = "subscribe_wanted/connection.rs"]
mod connection;
