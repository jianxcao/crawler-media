//! 强制探测刷新入队：手动整条目刷新与整季片头片尾刷新两条热路径。
//!
//! 纯搬迁自 `probe_manager.rs`（task-84），方法签名、文档与实现逐字不变。

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Instant;

use domain::MediaKind;

use super::{ProbeEnqueueError, ProbeManager, ProbeUnit, marker_refresh_scope, single_season};

impl ProbeManager {
    /// Force a complete season refresh while leaving its current markers and
    /// chapter caches visible. Results replace the old markers only after all
    /// episodes finish successfully.
    pub fn enqueue_marker_refresh(&self, mut units: Vec<ProbeUnit>) -> usize {
        let Some(first) = units.first() else {
            return 0;
        };
        let media_id = first.row.file.media_id;
        let Ok((season, _)) = super::require_episode_coordinates(&first.row) else {
            return 0;
        };
        if units.iter().any(|unit| {
            unit.row.file.media_id != media_id
                || super::require_single_unit(&unit.row).is_err()
                || single_season(&unit.row) != Some(season)
        }) {
            tracing::error!(%media_id, season, "拒绝创建跨媒体或跨季的片头片尾刷新批次");
            return 0;
        }

        let id = self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed);
        let episode_count = units.len();
        let started_at = Instant::now();
        let job_id = uuid::Uuid::new_v4().to_string();
        let scope = marker_refresh_scope(media_id, season);
        let ledger_ids = units
            .iter()
            .map(|unit| unit.row.file.file_id.to_string())
            .collect::<Vec<_>>();
        let specs = units
            .iter()
            .zip(&ledger_ids)
            .map(|(unit, ledger_id)| crate::store::ProbeJobUnitSpec {
                ledger_id,
                kind: unit.kind.as_str(),
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
            })
            .collect::<Vec<_>>();
        let Ok(store) = self.writer() else {
            return 0;
        };
        let created = store.create_probe_job(
            &job_id,
            "marker_refresh",
            &media_id.to_string(),
            Some(season),
            &scope,
            &specs,
        );
        drop(store);
        match created {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(%media_id, season, "该剧该季已有片头片尾刷新任务，拒绝重复入队");
                return 0;
            }
            Err(error) => {
                tracing::error!(%error, %media_id, season, "创建持久化片头片尾刷新任务失败");
                return 0;
            }
        }

        let mut queued = 0;
        for unit in &mut units {
            unit.force_fingerprint = true;
            unit.reuse_fingerprint_cache = false;
            unit.reuse_media_info_cache = true;
            unit.overwrite_markers = true;
            unit.marker_refresh_id = Some(id);
            unit.job_id = Some(job_id.clone());
            self.seen.lock().insert(unit.row.file.file_id.to_string());
            if self.queue_metadata(unit.clone()).is_ok() {
                queued += 1;
            } else {
                self.seen.lock().remove(&unit.row.file.file_id.to_string());
                tracing::info!(job_id, ledger_id = %unit.row.file.file_id, "刷新窗口已满，单元保持数据库排队");
            }
        }
        tracing::info!(%media_id, season, refresh_id = id, job_id, episodes = episode_count, queued,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "【片头片尾】已启动强制刷新，旧标记保持可见直到整季探测完成");
        queued
    }

    #[cfg(test)]
    pub(crate) fn enqueue_forced_item_refresh(
        &self,
        units: Vec<ProbeUnit>,
    ) -> Result<usize, ProbeEnqueueError> {
        self.enqueue_forced_item_refresh_checked(units, || Ok(()))
    }

    pub(crate) fn enqueue_forced_item_refresh_admitted(
        &self,
        units: Vec<ProbeUnit>,
        state: &crate::ApiState,
    ) -> Result<usize, ProbeEnqueueError> {
        self.enqueue_forced_item_refresh_checked(units, || {
            state
                .require_writable()
                .and_then(|_| state.require_write_admitted())
                .map_err(ProbeEnqueueError::Admission)
        })
    }

    /// Persist the whole item after admission is rechecked under the Store lock.
    fn enqueue_forced_item_refresh_checked(
        &self,
        mut units: Vec<ProbeUnit>,
        admitted: impl FnOnce() -> Result<(), ProbeEnqueueError>,
    ) -> Result<usize, ProbeEnqueueError> {
        let Some(first) = units.first() else {
            return Ok(0);
        };
        let media_id = first.row.file.media_id;
        let kind = first.kind;
        if units
            .iter()
            .any(|unit| unit.row.file.media_id != media_id || unit.kind != kind)
        {
            tracing::error!(%media_id, "拒绝创建跨媒体或媒体类型不一致的手动探测任务");
            return Err(ProbeEnqueueError::Persistence);
        }

        let is_tv = kind == MediaKind::Tv;
        if is_tv
            && units
                .iter()
                .any(|unit| super::require_single_unit(&unit.row).is_err())
        {
            return Err(ProbeEnqueueError::Persistence);
        }
        let job_kind = if is_tv {
            "marker_refresh"
        } else {
            "media_probe"
        };
        let scope = if is_tv {
            format!("marker-refresh:{media_id}:all")
        } else {
            format!("manual-probe:{media_id}")
        };
        let seasons = units
            .iter()
            .filter_map(|unit| single_season(&unit.row))
            .collect::<HashSet<_>>();
        let season = (seasons.len() == 1)
            .then(|| seasons.iter().next().copied())
            .flatten();
        let job_id = uuid::Uuid::new_v4().to_string();
        let ledger_ids = units
            .iter()
            .map(|unit| unit.row.file.file_id.to_string())
            .collect::<Vec<_>>();
        let specs = units
            .iter()
            .zip(&ledger_ids)
            .map(|(unit, ledger_id)| crate::store::ProbeJobUnitSpec {
                ledger_id,
                kind: unit.kind.as_str(),
                force_fingerprint: is_tv,
                reuse_fingerprint_cache: false,
                overwrite_markers: is_tv,
                reuse_media_info_cache: false,
            })
            .collect::<Vec<_>>();
        self.persist_item_refresh(
            &job_id, job_kind, media_id, season, &scope, &specs, admitted,
        )?;

        let refresh_id = is_tv.then(|| self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed));
        let mut queued = 0;
        for unit in &mut units {
            unit.force_fingerprint = is_tv;
            unit.reuse_fingerprint_cache = false;
            unit.reuse_media_info_cache = false;
            unit.overwrite_markers = is_tv;
            unit.marker_refresh_id = refresh_id;
            unit.job_id = Some(job_id.clone());
            self.seen.lock().insert(unit.row.file.file_id.to_string());
            if self.queue_metadata(unit.clone()).is_ok() {
                queued += 1;
            } else {
                self.seen.lock().remove(&unit.row.file.file_id.to_string());
                tracing::info!(job_id, ledger_id = %unit.row.file.file_id, "整条目窗口已满，单元保持数据库排队");
            }
        }
        tracing::info!(
            %media_id,
            job_id,
            seasons = seasons.len(),
            queued,
            total = units.len(),
            "【媒体探测】手动整条目刷新已持久化入队，旧结果保持可见"
        );
        Ok(queued)
    }
    fn persist_item_refresh(
        &self,
        job_id: &str,
        job_kind: &str,
        media_id: domain::MediaId,
        season: Option<u32>,
        scope: &str,
        specs: &[crate::store::ProbeJobUnitSpec<'_>],
        admitted: impl FnOnce() -> Result<(), ProbeEnqueueError>,
    ) -> Result<(), ProbeEnqueueError> {
        let Some(store) = self
            .store
            .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
        else {
            return Err(ProbeEnqueueError::Admission(
                crate::http::store_wait::busy_response(),
            ));
        };
        admitted()?;
        match store.create_probe_job(
            job_id,
            job_kind,
            &media_id.to_string(),
            season,
            scope,
            specs,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => {
                let conflict = specs.iter().any(|spec| {
                    store
                        .active_probe_unit_for_ledger(spec.ledger_id)
                        .ok()
                        .flatten()
                        .is_some()
                });
                Err(if conflict {
                    ProbeEnqueueError::AlreadyRunning
                } else {
                    ProbeEnqueueError::Persistence
                })
            }
            Err(error) => {
                tracing::error!(%error, %media_id, "创建手动媒体探测任务失败");
                Err(ProbeEnqueueError::Persistence)
            }
        }
    }
}
