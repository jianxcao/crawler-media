use domain::{Coverage, DownloaderId, MediaId, Release, Subscribe, SubscribeId, Torrent, UserId};
use rusqlite::{OptionalExtension, params};
use std::str::FromStr;
use subscribe::{QualityFact, SubscribeFacts};

use super::maps::{map_subscribe, torrent_from_json};
use super::{Store, StoreError};

pub struct PendingDownload {
    pub torrent: Torrent,
    pub release_override: Option<Release>,
    /// The Downloader that accepted this Torrent, retained across default changes.
    pub downloader_id: Option<DownloaderId>,
    /// 投递给下载器的时间（unix secs；resource_timing 用）。
    pub submitted_at: Option<i64>,
}

fn release_override_json(release: &Release) -> serde_json::Value {
    serde_json::json!({
        "title": release.title,
        "year": release.year,
        "season": release.season,
        "episode": release.episode,
        "episode_to": release.episode_to,
        "resolution": release.resolution,
        "source": release.source,
        "codec": release.codec,
        "hdr": release.hdr,
    })
}

fn release_from_json(value: Option<&serde_json::Value>) -> Option<Release> {
    let value = value?;
    Some(Release {
        title: value["title"].as_str().unwrap_or_default().to_string(),
        year: value["year"].as_u64().map(|n| n as u16),
        season: value["season"].as_u64().map(|n| n as u32),
        episode: value["episode"].as_u64().map(|n| n as u32),
        episode_to: value["episode_to"].as_u64().map(|n| n as u32),
        resolution: value["resolution"].as_str().map(str::to_string),
        source: value["source"].as_str().map(str::to_string),
        codec: value["codec"].as_str().map(str::to_string),
        hdr: value["hdr"].as_str().map(str::to_string),
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: domain::Confidence::High,
        season_inferred: false,
    })
}

fn pending_torrent_json(pending: &PendingDownload) -> String {
    let mut value = serde_json::json!({
        "site_id": pending.torrent.site_id.to_string(),
        "title": pending.torrent.title,
        "enclosure": pending.torrent.enclosure,
        "size_bytes": pending.torrent.size_bytes,
        "seeders": pending.torrent.seeders,
        "free": pending.torrent.free,
        "hr": pending.torrent.hr,
        "imdb_id": pending.torrent.imdb_id,
        "id": pending.torrent.id,
        "leechers": pending.torrent.leechers,
        "snatched": pending.torrent.snatched,
        "upload_time": pending.torrent.upload_time,
        "detail_url": pending.torrent.detail_url,
        "category": pending.torrent.category,
        "downloader_id": pending.downloader_id.map(|id| id.to_string()),
    });
    if let Some(release) = &pending.release_override {
        value["release_override"] = release_override_json(release);
    }
    value.to_string()
}

impl Store {
    pub fn list_subscribes_for_user(&self, user_id: UserId) -> Result<Vec<Subscribe>, StoreError> {
        self.with_subscribe_read(|db| db.list_subscribes_for_user(user_id))
    }

    pub fn list_all_subscribes(&self) -> Result<Vec<Subscribe>, StoreError> {
        self.with_subscribe_read(|db| db.list_all_subscribes())
    }

    /// 增量维护 pending：**绝不删除**已有行，只 upsert 本轮新候选（按
    /// (subscribe_id, enclosure) 主键）。正在下载/做种的任务在下一轮搜索后
    /// 仍保持跟踪；只有 transfer 成功（delete_pending）或显式取消时才删除。
    pub fn merge_pending(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.merge_pending(subscribe_id, torrents))
    }

    /// 明确记录一个由下载器提交成功的 pending 任务（更新 submitted_at 为当前时间，重置其 15 分钟清理宽限期）。
    pub fn record_pending_submission(
        &self,
        subscribe_id: SubscribeId,
        score: i32,
        pending: &PendingDownload,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.record_pending_submission(subscribe_id, score, pending))
    }

    /// 批量记录由下载器提交成功的 pending 任务（更新 submitted_at 为当前时间，重置其宽限期）。
    pub fn record_pending_submissions(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.record_pending_submissions(subscribe_id, torrents))
    }

    /// Explicit user submissions (manual admit): revive `rejected` rows (H2).
    pub fn record_manual_pending_submissions(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.record_manual_pending_submissions(subscribe_id, torrents))
    }

    /// Confirm only an existing unconfirmed delivery intent; never recreate or revive it.
    pub fn confirm_pending_submission(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        submitted_at: i64,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| {
            db.confirm_pending_submission(subscribe_id, enclosure, submitted_at)
        })
    }

    /// Mark wash-cut arrivals vetoed by their probed quality (H2).
    pub fn mark_pending_rejected(
        &self,
        subscribe_id: SubscribeId,
        enclosures: &[String],
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.mark_pending_rejected(subscribe_id, enclosures))
    }

    /// Enclosures automatic rounds must never pick again (H2).
    pub fn load_rejected_enclosures(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        self.with_subscribe_read(|db| db.load_rejected_enclosures(subscribe_id))
    }

    /// D-L4: active pending rows routed to a deleted downloader become
    /// `orphan`: Transfer stops polling them, and (unlike `rejected`) the
    /// release may be picked again for another downloader.
    pub fn mark_pending_orphaned(&self, downloader_id: DownloaderId) -> Result<usize, StoreError> {
        self.with_subscribe_write(|db| db.mark_pending_orphaned(downloader_id))
    }

    pub fn delete_pending(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.delete_pending(subscribe_id, enclosure))
    }

    pub fn load_pending(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<Vec<(i32, PendingDownload)>, StoreError> {
        self.with_subscribe_read(|db| db.load_pending(subscribe_id))
    }

    /// 按 state 取 pending：'active' 在途 / 'imported' 已入库（保留源链接，
    /// 库文件缺失时 transfer 据此重新硬链接）。
    pub fn load_pending_state(
        &self,
        subscribe_id: SubscribeId,
        state: &str,
    ) -> Result<Vec<(i32, PendingDownload)>, StoreError> {
        self.with_subscribe_read(|db| db.load_pending_state(subscribe_id, state))
    }

    /// 转存成功后把 pending 标记为 imported（不删除）——保留「哪个种子 → 哪个库文件」
    /// 的源链接（当前仅作留档，不自动重新收集）。
    pub fn mark_pending_imported(
        &self,
        subscribe_id: SubscribeId,
        enclosures: &[String],
    ) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.mark_pending_imported(subscribe_id, enclosures))
    }

    pub fn list_all_pending_routed(
        &self,
    ) -> Result<Vec<(SubscribeId, i32, PendingDownload)>, StoreError> {
        self.with_subscribe_read(|db| db.list_all_pending_routed())
    }

    pub fn list_all_pending(&self) -> Result<Vec<(SubscribeId, i32, Torrent)>, StoreError> {
        self.with_subscribe_read(|db| db.list_all_pending())
    }

    pub fn delete_subscribe(&self, id: SubscribeId) -> Result<bool, StoreError> {
        let deleted = self.with_writer(crate::Db::Subscribe, |subscribe| {
            let tx = crate::write_tx(subscribe)?;
            for table in ["pending_downloads", "subscribe_facts", "subscribe_wanted"] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE subscribe_id = ?1"),
                    params![id.to_string()],
                )?;
            }
            let n = tx.execute(
                "DELETE FROM subscribes WHERE id = ?1",
                params![id.to_string()],
            )?;
            tx.commit()?;
            Ok(n > 0)
        })?;
        // L4: per-subscription settings (`subscribe.<name>:<id>`, e.g. the
        // completion notice marker) live in the app database; drop them too.
        self.with_app(|app| {
            app.app.execute(
                "DELETE FROM settings WHERE key LIKE 'subscribe.%:' || ?1",
                params![id.to_string()],
            )?;
            Ok(())
        })?;
        Ok(deleted)
    }

    pub fn set_playback_progress(
        &self,
        user_id: UserId,
        media_id: MediaId,
        position_ms: i64,
    ) -> Result<(), StoreError> {
        self.upsert_unit(
            user_id,
            media_id,
            crate::UNIT_WHOLE,
            crate::UNIT_WHOLE,
            position_ms,
            None,
            None,
            None,
            None,
            None,
            false,
            super::unix_now(),
        )?;
        Ok(())
    }

    /// Upsert playback state: position + optional played/favorite/season/episode.
    /// `played`/`favorite` as None keep the stored value (partial update).
    #[allow(clippy::too_many_arguments)]
    pub fn set_playback_state(
        &self,
        user_id: UserId,
        media_id: MediaId,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<(), StoreError> {
        let (season, episode) = match (season, episode) {
            (Some(s), Some(e)) => (s as i32, e as i32),
            _ => (crate::UNIT_WHOLE, crate::UNIT_WHOLE),
        };
        self.upsert_unit(
            user_id,
            media_id,
            season,
            episode,
            position_ms,
            played,
            favorite,
            None,
            None,
            None,
            false,
            super::unix_now(),
        )?;
        Ok(())
    }

    /// Media-level marks: any played / any favorite across the user's units.
    pub fn playback_marks(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Option<(bool, bool)>, StoreError> {
        self.with_library_read(|db| db.playback_marks(user_id, media_id))
    }

    /// Resume position for a specific unit (season, episode) of a media.
    pub fn playback_unit_progress(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Option<i64>, StoreError> {
        self.with_library_read(|db| db.playback_unit_progress(user_id, media_id, season, episode))
    }

    /// Resume position for a media: the farthest position across units.
    pub fn playback_progress(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Option<i64>, StoreError> {
        self.with_library_read(|db| db.playback_progress(user_id, media_id))
    }

    pub fn insert_subscribe(&self, subscribe: &Subscribe) -> Result<(), StoreError> {
        self.insert_subscribe_checked(subscribe, false)
    }

    /// Decide coverage admission and insert while owning the Subscribe writer.
    pub fn insert_nonoverlapping_subscribe(&self, subscribe: &Subscribe) -> Result<(), StoreError> {
        self.insert_subscribe_checked(subscribe, true)
    }

    fn insert_subscribe_checked(
        &self,
        subscribe: &Subscribe,
        check_overlap: bool,
    ) -> Result<(), StoreError> {
        self.with_writer(crate::Db::Subscribe, |subscribe_conn| {
            let tx = crate::write_tx(subscribe_conn)?;
            if check_overlap {
                admission::ensure_nonoverlapping(&tx, subscribe)?;
            }
            self.with_writer(crate::Db::Library, |library| {
                admission::insert_row(library, &tx, subscribe)
            })?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_subscribe(&self, id: SubscribeId) -> Result<Option<Subscribe>, StoreError> {
        self.with_subscribe_read(|db| db.get_subscribe(id))
    }

    pub fn update_subscribe(&self, subscribe: &Subscribe) -> Result<(), StoreError> {
        self.with_subscribe_write(|db| db.update_subscribe(subscribe))
    }

    /// Decide coverage admission and update while owning the Subscribe writer.
    pub fn update_nonoverlapping_subscribe(&self, subscribe: &Subscribe) -> Result<(), StoreError> {
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let db = crate::SubscribeConnection { subscribe: &tx };
            let previous = db.get_subscribe(subscribe.id)?;
            if previous.as_ref().is_some_and(|previous| {
                previous.coverage != subscribe.coverage
                    || previous.library_id != subscribe.library_id
            }) {
                admission::ensure_nonoverlapping(&tx, subscribe)?;
            }
            db.update_subscribe(subscribe)?;
            tx.commit()?;
            Ok(())
        })
    }

    /// `(created_at, updated_at)` RFC3339 for an existing Subscribe.
    pub fn subscribe_times(&self, id: SubscribeId) -> Result<(String, String), StoreError> {
        self.with_subscribe_read(|db| db.subscribe_times(id))
    }

    /// 把内存里的订阅事实写回 DB。内存层已是唯一判定点：`upsert` 只接受
    /// 更高分（防陈旧 pending 回退），`replace` 写入 chooser 批准的替换
    /// （含 UpgradeLadder 同分/低分更高维度）。这里必须**无条件镜像**内存值：
    /// 早期版本在 SQL 里再做一次 `excluded.score > score` 守门，导致洗版
    /// 批准的同分替换（新文件已入库、旧文件已回收）写不进去，DB 永远指向
    /// 已被删除的旧路径，下一轮重复批准洗版。
    pub fn save_subscribe_facts(
        &self,
        subscribe_id: SubscribeId,
        facts: &SubscribeFacts,
    ) -> Result<(), StoreError> {
        self.persist_owned_quality(facts)?;
        self.with_writer(crate::Db::Subscribe, |subscribe| {
            let tx = crate::write_tx(subscribe)?;
            let mut statement = tx.prepare(
                "INSERT INTO subscribe_facts (subscribe_id, season, episode, score, path)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(subscribe_id, season, episode) DO UPDATE SET
               score = excluded.score,
               path = excluded.path",
            )?;
            for ((season, episode), fact) in facts.entries().filter(|(_, fact)| fact.path.is_some())
            {
                statement.execute(params![
                    subscribe_id.to_string(),
                    season.map_or(-1i64, |s| s as i64),
                    episode.map_or(-1i64, |e| e as i64),
                    fact.score,
                    fact.path,
                ])?;
            }
            drop(statement);
            tx.execute(
                "DELETE FROM subscribe_facts WHERE subscribe_id = ?1 AND path IS NULL",
                [subscribe_id.to_string()],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_subscribe_facts(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<SubscribeFacts, StoreError> {
        let rows = self.with_read(crate::Db::Subscribe, |subscribe| {
            let mut statement = subscribe.prepare(
                "SELECT season, episode, score, path
             FROM subscribe_facts WHERE subscribe_id = ?1 AND path IS NOT NULL",
            )?;
            let rows = statement.query_map(params![subscribe_id.to_string()], |row| {
                let season: i64 = row.get(0)?;
                let episode: i64 = row.get(1)?;
                Ok((
                    (season >= 0).then_some(season as u32),
                    (episode >= 0).then_some(episode as u32),
                    QualityFact {
                        score: row.get(2)?,
                        path: row.get(3)?,
                    },
                ))
            })?;
            let rows = rows.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let mut facts = SubscribeFacts::default();
        for (season, episode, fact) in rows {
            if let Some(path) = fact.path.as_deref()
                && let Some(coverage) = self.file_coverage_by_path(path)?
            {
                facts.set_quality(path.to_string(), self.owned_quality(&coverage)?);
            }
            facts.upsert(season, episode, fact);
        }
        Ok(facts)
    }
}

impl Store {
    /// Subscription ownership roster for in-memory job filtering.
    pub fn subscribe_ids_for_user(
        &self,
        user_id: domain::UserId,
    ) -> Result<std::collections::HashSet<domain::SubscribeId>, StoreError> {
        self.with_subscribe_read(|db| db.subscribe_ids_for_user(user_id))
    }
}

#[path = "subscribes/connection.rs"]
mod connection;

mod admission;
mod media_batch;
