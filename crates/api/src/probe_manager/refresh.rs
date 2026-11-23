//! 强制探测刷新入队：手动整条目刷新与整季片头片尾刷新两条热路径。
//!
//! 纯搬迁自 `probe_manager.rs`（task-84），方法签名、文档与实现逐字不变。

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Instant;

use domain::MediaKind;

use super::{ProbeEnqueueError, ProbeManager, ProbeUnit, marker_refresh_scope, single_season_opt};

impl ProbeManager {
    /// Force a complete season refresh while leaving its current markers and
    /// chapter caches visible. Results replace the old markers only after all
    /// episodes finish successfully.
    pub fn enqueue_marker_refresh(&self, units: Vec<ProbeUnit>) -> usize {
        self.enqueue_marker_refresh_with_policy(
            units,
            crate::fingerprint_job::CapturePolicy::Recapture,
        )
    }

    pub fn enqueue_marker_refresh_with_policy(
        &self,
        mut units: Vec<ProbeUnit>,
        capture_policy: crate::fingerprint_job::CapturePolicy,
    ) -> usize {
        let reuse = capture_policy == crate::fingerprint_job::CapturePolicy::ReuseValid;
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
                || single_season_opt(&unit.row) != Some(season)
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
                reuse_fingerprint_cache: reuse,
                overwrite_markers: true,
                reuse_media_info_cache: true,
            })
            .collect::<Vec<_>>();
        if !self.persist_marker_refresh_job(&job_id, media_id, season, &scope, &ledger_ids, &specs)
        {
            return 0;
        }
        self.mark_job_paused_exempt(job_id.clone());

        let mut queued = 0;
        let mut overflowed = false;
        for unit in &mut units {
            unit.force_fingerprint = true;
            unit.reuse_fingerprint_cache = reuse;
            unit.reuse_media_info_cache = true;
            unit.overwrite_markers = true;
            unit.marker_refresh_id = Some(id);
            unit.job_id = Some(job_id.clone());
            if self.should_defer_remote_dispatch(&unit.row.file.path) {
                tracing::info!(job_id, ledger_id = %unit.row.file.file_id, "远程读取已暂停，刷新单元保持数据库排队");
            } else {
                let ledger_id = unit.row.file.file_id.to_string();
                if self.try_insert_seen(ledger_id.clone(), job_id.clone()) {
                    if self.queue_metadata(unit.clone()).is_ok() {
                        queued += 1;
                    } else {
                        overflowed = true;
                        self.remove_seen_if_job(&ledger_id, &job_id);
                        tracing::info!(job_id, ledger_id, "刷新窗口已满，单元保持数据库排队");
                    }
                }
            }
        }
        if overflowed {
            self.wake.notify_waiters();
        }
        tracing::info!(%media_id, season, refresh_id = id, job_id, episodes = episode_count, queued,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "【片头片尾】已启动强制刷新，旧标记保持可见直到整季探测完成");
        queued
    }

    fn persist_marker_refresh_job(
        &self,
        job_id: &str,
        media_id: domain::MediaId,
        season: u32,
        scope: &str,
        ledger_ids: &[String],
        specs: &[crate::store::ProbeJobUnitSpec<'_>],
    ) -> bool {
        enum RefreshPersistErr {
            Conflict,
            Store(crate::store::StoreError),
        }
        let paused = self.background_is_paused();
        let exempt_jobs = if paused {
            self.paused_exempt_jobs_for_ledgers(ledger_ids)
        } else {
            HashSet::new()
        };
        let outcome = self
            .store
            .library_write_with_error(RefreshPersistErr::Store, |store| {
                store.with_write_tx(RefreshPersistErr::Store, |store| {
                    let cancelled = if paused {
                        store
                            .cancel_dormant_queued_units_for_ledgers(
                                ledger_ids,
                                &exempt_jobs,
                                "superseded_by_manual_probe",
                            )
                            .map_err(RefreshPersistErr::Store)?
                    } else {
                        Vec::new()
                    };
                    let created = store
                        .create_probe_job(
                            job_id,
                            "marker_refresh",
                            &media_id.to_string(),
                            Some(season),
                            scope,
                            specs,
                        )
                        .map_err(RefreshPersistErr::Store)?;
                    if !created {
                        return Err(RefreshPersistErr::Conflict);
                    }
                    Ok(cancelled)
                })
            });
        match outcome {
            Ok(cancelled) => {
                if !cancelled.is_empty() {
                    let mut seen = self.seen.lock();
                    for id in cancelled {
                        seen.remove(&id);
                    }
                }
                true
            }
            Err(RefreshPersistErr::Conflict) => {
                tracing::info!(%media_id, season, "该剧该季已有片头片尾刷新任务，拒绝重复入队");
                false
            }
            Err(RefreshPersistErr::Store(error)) => {
                tracing::error!(%error, %media_id, season, "创建持久化片头片尾刷新任务失败");
                false
            }
        }
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

    /// Recheck admission and persist the whole item in one Library transaction.
    fn enqueue_forced_item_refresh_checked(
        &self,
        units: Vec<ProbeUnit>,
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
        let batches = build_item_refresh_batches(media_id, is_tv, units);
        self.persist_item_refresh_batches(media_id, &batches, admitted)?;
        for batch in &batches {
            self.mark_job_paused_exempt(batch.job_id.clone());
        }

        let mut queued = 0;
        let mut total = 0;
        let mut overflowed = false;
        let mut seasons = HashSet::new();
        for mut batch in batches {
            let refresh_id = batch
                .is_marker
                .then(|| self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed));
            total += batch.units.len();
            for unit in &mut batch.units {
                if let Some(season) = single_season_opt(&unit.row) {
                    seasons.insert(season);
                }
                unit.force_fingerprint = batch.is_marker;
                unit.reuse_fingerprint_cache = false;
                unit.reuse_media_info_cache = false;
                unit.overwrite_markers = batch.is_marker;
                unit.marker_refresh_id = refresh_id;
                unit.job_id = Some(batch.job_id.clone());
                if self.should_defer_remote_dispatch(&unit.row.file.path) {
                    tracing::info!(job_id = %batch.job_id, ledger_id = %unit.row.file.file_id, "远程读取已暂停，整条目单元保持数据库排队");
                } else {
                    let ledger_id = unit.row.file.file_id.to_string();
                    if self.try_insert_seen(ledger_id.clone(), batch.job_id.clone()) {
                        if self.queue_metadata(unit.clone()).is_ok() {
                            queued += 1;
                        } else {
                            overflowed = true;
                            self.remove_seen_if_job(&ledger_id, &batch.job_id);
                            tracing::info!(job_id = %batch.job_id, ledger_id, "整条目窗口已满，单元保持数据库排队");
                        }
                    }
                }
            }
        }
        if overflowed {
            self.wake.notify_waiters();
        }
        tracing::info!(
            %media_id,
            seasons = seasons.len(),
            queued,
            total,
            "【媒体探测】手动整条目刷新已持久化入队，旧结果保持可见"
        );
        Ok(queued)
    }

    fn persist_item_refresh_batches(
        &self,
        media_id: domain::MediaId,
        batches: &[ItemRefreshBatch],
        admitted: impl FnOnce() -> Result<(), ProbeEnqueueError>,
    ) -> Result<(), ProbeEnqueueError> {
        let paused = self.background_is_paused();
        let target_ids: Vec<String> = batches
            .iter()
            .flat_map(|batch| {
                batch
                    .units
                    .iter()
                    .map(|unit| unit.row.file.file_id.to_string())
            })
            .collect();
        let exempt_jobs = if paused {
            self.paused_exempt_jobs_for_ledgers(&target_ids)
        } else {
            HashSet::new()
        };
        let map_store_err = |error: crate::store::StoreError| {
            if super::store_access::is_busy(&error) {
                ProbeEnqueueError::Admission(crate::http::store_wait::busy_response())
            } else {
                tracing::error!(%error, %media_id, "创建手动媒体探测任务失败");
                ProbeEnqueueError::Persistence
            }
        };
        let cancelled = self
            .store
            .library_write_with_error(map_store_err, |store| {
                admitted()?;
                store.with_write_tx(map_store_err, |store| {
                    let cancelled = if paused {
                        store
                            .cancel_dormant_queued_units_for_ledgers(
                                &target_ids,
                                &exempt_jobs,
                                "superseded_by_manual_probe",
                            )
                            .map_err(map_store_err)?
                    } else {
                        Vec::new()
                    };
                    for batch in batches {
                        let ledger_ids = batch
                            .units
                            .iter()
                            .map(|unit| unit.row.file.file_id.to_string())
                            .collect::<Vec<_>>();
                        let specs = batch
                            .units
                            .iter()
                            .zip(&ledger_ids)
                            .map(|(unit, ledger_id)| crate::store::ProbeJobUnitSpec {
                                ledger_id,
                                kind: unit.kind.as_str(),
                                force_fingerprint: batch.is_marker,
                                reuse_fingerprint_cache: false,
                                overwrite_markers: batch.is_marker,
                                reuse_media_info_cache: false,
                            })
                            .collect::<Vec<_>>();
                        match store.create_probe_job(
                            &batch.job_id,
                            batch.job_kind,
                            &media_id.to_string(),
                            batch.season,
                            &batch.scope,
                            &specs,
                        ) {
                            Ok(true) => {}
                            Ok(false) => {
                                let conflict = specs.iter().any(|spec| {
                                    store
                                        .active_probe_unit_for_ledger(spec.ledger_id)
                                        .ok()
                                        .flatten()
                                        .is_some()
                                });
                                return Err(if conflict {
                                    ProbeEnqueueError::AlreadyRunning
                                } else {
                                    ProbeEnqueueError::Persistence
                                });
                            }
                            Err(error) => {
                                tracing::error!(%error, %media_id, "创建手动媒体探测任务失败");
                                return Err(ProbeEnqueueError::Persistence);
                            }
                        }
                    }
                    Ok(cancelled)
                })
            })?;
        if !cancelled.is_empty() {
            let mut seen = self.seen.lock();
            for id in cancelled {
                seen.remove(&id);
            }
        }
        Ok(())
    }
}

struct ItemRefreshBatch {
    job_id: String,
    job_kind: &'static str,
    season: Option<u32>,
    scope: String,
    is_marker: bool,
    units: Vec<ProbeUnit>,
}

fn build_item_refresh_batches(
    media_id: domain::MediaId,
    is_tv: bool,
    units: Vec<ProbeUnit>,
) -> Vec<ItemRefreshBatch> {
    if !is_tv {
        return vec![make_item_refresh_batch(
            "media_probe",
            format!("manual-probe:{media_id}"),
            false,
            units,
        )];
    }
    let (marker_units, media_only_units): (Vec<_>, Vec<_>) = units
        .into_iter()
        .partition(|unit| super::require_single_unit(&unit.row).is_ok());
    let mut batches = Vec::new();
    if !marker_units.is_empty() {
        batches.push(make_item_refresh_batch(
            "marker_refresh",
            format!("marker-refresh:{media_id}:all"),
            true,
            marker_units,
        ));
    }
    if !media_only_units.is_empty() {
        batches.push(make_item_refresh_batch(
            "media_probe",
            format!("manual-probe:{media_id}"),
            false,
            media_only_units,
        ));
    }
    batches
}

fn make_item_refresh_batch(
    job_kind: &'static str,
    scope: String,
    is_marker: bool,
    units: Vec<ProbeUnit>,
) -> ItemRefreshBatch {
    let seasons = units
        .iter()
        .filter_map(|unit| single_season_opt(&unit.row))
        .collect::<HashSet<_>>();
    let season = (seasons.len() == 1)
        .then(|| seasons.iter().next().copied())
        .flatten();
    ItemRefreshBatch {
        job_id: uuid::Uuid::new_v4().to_string(),
        job_kind,
        season,
        scope,
        is_marker,
        units,
    }
}
