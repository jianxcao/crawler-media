//! Durable probe completion and cancellation.

use std::collections::BTreeSet;

#[cfg(test)]
use super::synthetic_coverage;
use super::{ProbeManager, ProbeUnit, now_ms, single_episode, timings};
use domain::MediaId;

impl ProbeManager {
    pub(super) fn reconcile_marker_refresh_after_retirement(
        &self,
        job_id: &str,
    ) -> Result<(), store::StoreError> {
        if let Some(failed_job) = self
            .store
            .fail_probe_analysis_refresh_if_capture_failed(job_id)?
        {
            self.unmark_job_paused_exempt(&failed_job.id);
            timings::finalize_terminal_job(&self.store_access(), &self.timings, &failed_job);
            return Ok(());
        }
        let Some(participants) = self.store.probe_analysis_job_participants(job_id)? else {
            return Ok(());
        };
        if self.background_is_paused() {
            let job_exempt = self.is_job_paused_exempt(job_id);
            let promoted_survivor = participants
                .iter()
                .find(|p| {
                    let lid = &p.dispatch().ledger_id;
                    self.is_unit_paused_exempt(job_id, lid) || self.was_unit_promoted(job_id, lid)
                })
                .map(|p| p.dispatch().ledger_id.clone());
            if !job_exempt {
                let Some(lid) = promoted_survivor else {
                    return Ok(());
                };
                self.mark_unit_paused_exempt(job_id.to_string(), lid);
            }
        }
        super::analysis::observe_dirty_participants(self, &participants)?;
        self.wake.notify_waiters();
        Ok(())
    }

    pub(super) fn purge_cancelled_seen_inner(&self, file_ids: &[String], global_cancel: bool) {
        if global_cancel {
            self.analysis_schedule.lock().clear_unstarted();
            self.drain_buffered_dispatch();
        }
        self.prune_exemptions_and_reconcile_promoted_from_store();
        self.reconcile_terminal_active_timings();
        self.evict_inactive_seen_for_files(file_ids);
        if !self.background_is_paused() || self.has_paused_exempt_work() {
            self.wake.notify_waiters();
        }
    }

    pub(super) fn reconcile_terminal_active_timings(&self) {
        for job_id in self.timings.active_job_ids() {
            match self.store.get_probe_job(&job_id) {
                Ok(Some(job)) if !job.is_active() => {
                    self.unmark_job_paused_exempt(&job.id);
                    timings::finalize_terminal_job(&self.store_access(), &self.timings, &job);
                }
                Ok(None) => {
                    self.unmark_job_paused_exempt(&job_id);
                    self.timings.drop_missing_job(&job_id);
                }
                _ => {}
            }
        }
    }

    fn prune_exemptions_and_reconcile_promoted_from_store(&self) {
        let exempt_jobs = self.paused_exempt_job_ids();
        let exempt_units = self.paused_exempt_unit_pairs();
        let promoted_units = self.promoted_unit_pairs();
        if !exempt_jobs.is_empty() || !exempt_units.is_empty() || !promoted_units.is_empty() {
            let mut checked_units = exempt_units.clone();
            for pair in &promoted_units {
                if !checked_units.contains(pair) {
                    checked_units.push(pair.clone());
                }
            }
            if let Ok(Ok((_, active_jobs, active_units))) =
                self.store.library_read_snapshot(|conn| {
                    conn.pending_probe_dispatch_ids_for_exemptions(
                        store::ProbeDispatchStage::Metadata,
                        &exempt_jobs,
                        &checked_units,
                        false,
                        0,
                    )
                })
            {
                self.prune_inactive_exempt_jobs(&exempt_jobs, &active_jobs);
                self.prune_inactive_exempt_units(&exempt_units, &active_units);
                self.prune_inactive_promoted_units(&promoted_units, &active_units);
            }
        }
        let surviving_promoted_jobs: BTreeSet<String> = self
            .promoted_unit_pairs()
            .into_iter()
            .map(|(job_id, _)| job_id)
            .collect();
        for job_id in surviving_promoted_jobs {
            if self
                .store
                .get_probe_job(&job_id)
                .ok()
                .flatten()
                .is_some_and(|job| job.is_active() && job.kind == "marker_refresh")
            {
                let _ = self.reconcile_marker_refresh_after_retirement(&job_id);
            }
        }
    }

    fn evict_inactive_seen_for_files(&self, file_ids: &[String]) {
        let candidates: Vec<(String, String)> = {
            let seen = self.seen.lock();
            file_ids
                .iter()
                .filter_map(|fid| seen.get(fid).map(|jid| (fid.clone(), jid.clone())))
                .collect()
        };
        if candidates.is_empty() {
            return;
        }
        let mut stale = Vec::new();
        for (fid, jid) in candidates {
            let keep = match self.store.active_probe_unit_for_ledger(&fid) {
                Ok(Some(active)) => active.job_id == jid,
                Ok(None) => self.store.get_probe_job(&jid).ok().flatten().is_none(),
                Err(_) => true,
            };
            if !keep {
                stale.push((fid, jid));
            }
        }
        if !stale.is_empty() {
            let mut seen = self.seen.lock();
            for (fid, jid) in stale {
                super::remove_seen_entry_if_job(&mut seen, &fid, &jid);
            }
        }
    }

    pub fn cancel_ledger_work(
        &self,
        ledger_ids: &[String],
        reason: &str,
    ) -> Result<(), store::StoreError> {
        let mut affected_refresh_jobs = BTreeSet::new();
        for ledger_id in ledger_ids {
            let unit = self.store.active_probe_unit_for_ledger(ledger_id)?;
            if let Some(unit) = unit {
                let job = self.store.library_write(|store| {
                    let (job, _, _) = store.cancel_probe_unit(&unit.job_id, ledger_id, reason)?;
                    Ok(job)
                })?;
                if !job.is_active() {
                    self.unmark_job_paused_exempt(&job.id);
                    timings::finalize_terminal_job(&self.store_access(), &self.timings, &job);
                } else {
                    self.unmark_unit_paused_exempt(&unit.job_id, ledger_id);
                    self.unmark_unit_promoted(&unit.job_id, ledger_id);
                    if job.kind == "marker_refresh" {
                        affected_refresh_jobs.insert(job.id);
                    }
                }
                self.remove_seen_if_job(ledger_id, &unit.job_id);
            }
        }
        for job_id in affected_refresh_jobs {
            self.reconcile_marker_refresh_after_retirement(&job_id)?;
        }
        Ok(())
    }

    /// 是否已在排队/执行（前端「探测中」状态用）。
    ///
    /// Uses the shared reader pool. Async callers use their existing off-executor
    /// read seam so reader acquisition cannot park a Tokio worker.
    pub fn is_queued(&self, ledger_id: &str) -> bool {
        let queued = self.store.is_probe_queued(ledger_id);
        match queued {
            Ok(queued) => queued,
            Err(_) => self.has_seen(ledger_id),
        }
    }

    /// True only while this ledger sits in the in-memory dispatch window.
    pub fn holds_dispatch_id(&self, ledger_id: &str) -> bool {
        self.has_seen(ledger_id)
    }

    pub(crate) fn marker_refresh_job(
        &self,
        media_id: MediaId,
        season: u32,
    ) -> Result<Option<crate::store::ProbeJob>, crate::store::StoreError> {
        self.store
            .latest_marker_refresh_for_season(&media_id.to_string(), season)
    }

    #[cfg(test)]
    pub(crate) fn take_queued_for_test(&self) -> Option<ProbeUnit> {
        let id = self.metadata_rx.lock().as_mut()?.try_recv().ok()?;
        self.remove_seen_if_job(&id.ledger_id, &id.job_id);

        if let Some((unit, _)) = self.load_dispatch_unit_inner(&id, false) {
            return Some(unit);
        }
        Some(ProbeUnit {
            row: synthetic_coverage(&id.ledger_id),
            kind: domain::MediaKind::Movie,
            force_fingerprint: false,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: Some(id.job_id),
        })
    }

    #[cfg(test)]
    pub(crate) fn take_fingerprint_for_test(&self) -> Option<ProbeUnit> {
        let (id, _work) = self.fingerprint_rx.lock().as_mut()?.try_recv().ok()?;
        if let Some((unit, _)) = self.load_dispatch_unit_inner(&id, false) {
            return Some(unit);
        }
        Some(ProbeUnit {
            row: synthetic_coverage(&id.ledger_id),
            kind: domain::MediaKind::Movie,
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: Some(id.job_id),
        })
    }

    #[cfg(test)]
    pub(crate) fn has_fingerprint_queued_for_test(&self) -> bool {
        self.fingerprint_rx
            .lock()
            .as_mut()
            .and_then(|rx| rx.try_recv().ok().map(|(id, _)| id))
            .is_some()
    }

    pub(super) fn finish(&self, unit: &ProbeUnit, succeeded: bool) {
        let status = if succeeded { "succeeded" } else { "failed" };
        self.finish_with_status(
            unit,
            status,
            None,
            (!succeeded).then_some("探测单元执行失败"),
        );
    }

    pub(crate) fn finish_with_status(
        &self,
        unit: &ProbeUnit,
        status: &str,
        error_kind: Option<&str>,
        error: Option<&str>,
    ) {
        let ledger_id = unit.row.file.file_id.to_string();
        let cancelled = unit
            .job_id
            .as_deref()
            .and_then(|id| self.store.get_probe_job(id).ok().flatten())
            .is_some_and(|job| job.status == "cancelled");
        if cancelled {
            if let Some(id) = unit.job_id.as_deref() {
                self.unmark_job_paused_exempt(id);
                self.remove_seen_if_job(&ledger_id, id);
            } else {
                self.remove_seen_any(&ledger_id);
            }
            return;
        }
        let persisted = self.finish_persisted_unit_with_status(unit, status, error_kind, error);
        if let Some(job_id) = unit.job_id.as_deref() {
            self.unmark_unit_paused_exempt(job_id, &ledger_id);
            self.unmark_unit_promoted(job_id, &ledger_id);
            self.remove_seen_if_job(&ledger_id, job_id);
        } else {
            self.remove_seen_any(&ledger_id);
        }
        if persisted && status == "failed" {
            self.mark_failed(&ledger_id);
        }
    }

    pub(crate) fn cancel_unit(&self, unit: &ProbeUnit, reason: &str) {
        let ledger_id = unit.row.file.file_id.to_string();
        let Some(job_id) = unit.job_id.as_deref() else {
            self.remove_seen_any(&ledger_id);
            return;
        };
        self.unmark_unit_paused_exempt(job_id, &ledger_id);
        self.unmark_unit_promoted(job_id, &ledger_id);
        self.remove_seen_if_job(&ledger_id, job_id);
        let result = self.store.library_write(|store| {
            store
                .cancel_probe_unit(job_id, &ledger_id, reason)
                .and_then(|(job, _, all_done)| {
                    if all_done && job.succeeded == 0 && job.failed == 0 && job.partial == 0 {
                        store.cancel_probe_job(job_id, reason)?;
                    }
                    store.get_probe_job(job_id)
                })
        });
        match result {
            Ok(Some(job)) => {
                if !job.is_active() {
                    self.unmark_job_paused_exempt(job_id);
                    // Enqueue holds seen before Store. Never retain the Store
                    // temporary guard while acquiring seen (including in if let).
                    let units = self.store.probe_job_units(job_id);
                    if let Ok(units) = units {
                        let mut seen = self.seen.lock();
                        for pending in units {
                            super::remove_seen_entry_if_job(&mut seen, &pending.ledger_id, job_id);
                        }
                    }
                    timings::log_terminal_job(&self.store_access(), &self.timings, &job);
                } else if job.kind == "marker_refresh"
                    && let Err(error) = self.reconcile_marker_refresh_after_retirement(job_id)
                {
                    tracing::warn!(%error, job_id, "取消刷新单元后同步分析唤醒失败");
                }
                tracing::info!(job_id, ledger_id, reason, status = %job.status,
                    elapsed_ms = job.elapsed_ms(now_ms()), "【媒体探测】任务已取消");
            }
            Ok(None) => tracing::error!(job_id, ledger_id, "取消时探测任务不存在"),
            Err(error) => tracing::error!(%error, job_id, ledger_id, "持久化探测任务取消状态失败"),
        }
    }

    pub(crate) fn finish_persisted_unit_with_status(
        &self,
        unit: &ProbeUnit,
        status: &str,
        error_kind: Option<&str>,
        error: Option<&str>,
    ) -> bool {
        let ledger_id = unit.row.file.file_id.to_string();
        let Some(job_id) = unit.job_id.as_deref() else {
            tracing::error!(ledger_id, "探测队列项缺少持久化任务 ID，拒绝写入结果");
            return false;
        };
        let uncertain = if error_kind == Some("verification_uncertain") {
            match super::uncertainty::prepare_json(
                self,
                unit,
                error.unwrap_or("verification_unresolved"),
            ) {
                Ok(json) => Some(json),
                Err(error) => {
                    tracing::error!(%error, job_id, ledger_id, "未决识别状态准备失败，保留任务");
                    return false;
                }
            }
        } else {
            None
        };
        let result = self.store.library_write(|store| {
            if let Some(json) = &uncertain {
                super::markers::validate_active_unit(store, unit)?;
                super::markers::validate_publication_rows(store, std::slice::from_ref(&unit.row))?;
                store.put_probe_detection_outcome(job_id, &ledger_id, json)?;
            }
            store.finish_probe_unit_with_status(job_id, &ledger_id, status, error_kind, error)
        });
        let (job, accepted, all_done) = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, ledger_id, job_id, "持久化探测任务结果失败");
                return false;
            }
        };
        if !accepted {
            tracing::warn!(job_id, ledger_id, "忽略已结束或重复提交的探测单元结果");
            return false;
        }
        tracing::info!(
            job_id = %job.id,
            media_id = %job.media_id,
            season = job.season.unwrap_or(1),
            episode = ?single_episode(&unit.row),
            completed = job.completed,
            total = job.total,
            status,
            "【媒体探测】任务单元结果已写入数据库"
        );
        if !all_done {
            if job.kind == "marker_refresh"
                && (status == "failed" || status == "partial" || job.failed > 0 || job.partial > 0)
            {
                self.fail_refresh_if_capture_failed(&job.id);
            }
            return true;
        }
        self.finalize_all_done_probe_job(&job);
        true
    }

    fn finalize_all_done_probe_job(&self, job: &store::ProbeJob) {
        if job.status == "cancelled" {
            self.unmark_job_paused_exempt(&job.id);
            timings::log_terminal_job(&self.store_access(), &self.timings, job);
            return;
        }
        if job.failed > 0 || job.status == "failed" {
            self.unmark_job_paused_exempt(&job.id);
            if job.is_active()
                && let Err(error) = self.store.library_write(|store| {
                    store.finish_probe_job(
                        &job.id,
                        false,
                        job.error.as_deref().or(Some("一个或多个探测单元失败")),
                    )
                })
            {
                tracing::error!(%error, job_id = %job.id, "持久化探测任务失败状态失败");
            }
            tracing::warn!(
                job_id = %job.id,
                media_id = %job.media_id,
                completed = job.completed,
                total = job.total,
                elapsed_ms = job.elapsed_ms(now_ms()),
                "【探测任务】部分单元失败，保留现有片头片尾结果"
            );
            timings::log_terminal_job(&self.store_access(), &self.timings, job);
        } else if job.kind == "marker_refresh" {
            if job.partial > 0 && self.fail_refresh_if_capture_failed(&job.id) {
                return;
            }
            self.wake.notify_waiters();
        } else {
            self.unmark_job_paused_exempt(&job.id);
            let final_succeeded = job.status == "succeeded";
            let job_final_status = job.status.clone();
            match self.store.library_write(|store| {
                store.finish_probe_job(&job.id, final_succeeded, job.error.as_deref())
            }) {
                Ok(()) => tracing::info!(
                    job_id = %job.id,
                    media_id = %job.media_id,
                    completed = job.completed,
                    total = job.total,
                    status = %job_final_status,
                    elapsed_ms = job.elapsed_ms(now_ms()),
                    "【媒体探测】持久化任务完成"
                ),
                Err(error) => {
                    tracing::error!(%error, job_id = %job.id, "持久化探测任务成功状态失败")
                }
            }
            timings::log_terminal_job(&self.store_access(), &self.timings, job);
        }
    }

    pub(super) fn fail_refresh_if_capture_failed(&self, job_id: &str) -> bool {
        match self
            .store
            .fail_probe_analysis_refresh_if_capture_failed(job_id)
        {
            Ok(Some(failed_job)) => {
                self.unmark_job_paused_exempt(&failed_job.id);
                timings::finalize_terminal_job(&self.store_access(), &self.timings, &failed_job);
                true
            }
            Ok(None) => false,
            Err(error) => {
                tracing::error!(%error, job_id, "部分采集失败后终止片头片尾刷新任务失败");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Media, MediaId, MediaKind};
    use std::sync::Arc;

    #[test]
    fn purge_cancelled_seen_preserves_concurrently_created_active_job_and_prunes_exemptions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let seed = |name: &str| {
            let path = root.join(name);
            std::fs::write(&path, b"fixture").unwrap();
            let media_id = MediaId::new();
            store
                .insert_media(&Media {
                    id: media_id,
                    kind: MediaKind::Movie,
                    title: name.into(),
                    year: None,
                    original_title: None,
                    aliases: vec![],
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                })
                .unwrap();
            super::super::test_support::publish(
                &store,
                &super::super::test_support::coverage(media_id, path.to_str().unwrap(), None, None),
            )
        };
        let row_replaced = seed("replaced.mkv");
        let row_exempt = seed("exempt.mkv");
        let manager = ProbeManager::new_paused(store.clone()).unwrap();

        // Both files get Playback-exempt jobs while paused.
        manager
            .request_probe(
                &row_replaced,
                super::super::policy::ProbeRequestOrigin::Playback,
            )
            .unwrap();
        manager
            .request_probe(
                &row_exempt,
                super::super::policy::ProbeRequestOrigin::Playback,
            )
            .unwrap();
        assert!(manager.has_paused_exempt_work());

        // SQLite cancel runs before purge_cancelled_seen; simulate a replacement job landing in between!
        let summary = store
            .cancel_queued_probe_work(None, "manual_cancel")
            .unwrap();
        manager.remove_seen_any(&row_replaced.file.file_id.to_string());
        manager
            .request_probe(
                &row_replaced,
                super::super::policy::ProbeRequestOrigin::Playback,
            )
            .unwrap();
        assert!(manager.holds_dispatch_id(&row_replaced.file.file_id.to_string()));

        manager.purge_cancelled_seen(&summary.cancelled_file_ids, true);
        // Wait: global_cancel drains buffered channels, so test non-global cancel for preserving seen!
        let summary2 = store
            .cancel_queued_probe_work(None, "manual_cancel")
            .unwrap();
        manager.purge_cancelled_seen(&summary2.cancelled_file_ids, true);
        assert!(!manager.has_paused_exempt_work());

        manager
            .request_probe(
                &row_replaced,
                super::super::policy::ProbeRequestOrigin::Playback,
            )
            .unwrap();
        let lid = row_replaced.file.file_id.to_string();
        assert!(manager.holds_dispatch_id(&lid));
        manager.purge_cancelled_seen(std::slice::from_ref(&lid), false);
        assert!(
            manager.holds_dispatch_id(&lid),
            "purge_cancelled_seen must not evict a newly-created active job from seen"
        );
    }
}
