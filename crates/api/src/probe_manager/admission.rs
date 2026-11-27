//! Probe policy assessment and durable admission.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{ProbeManager, ProbeUnit, policy, probe, single_season_opt};

impl ProbeManager {
    pub fn request_probe(
        &self,
        row: &domain::FileCoverage,
        origin: policy::ProbeRequestOrigin,
    ) -> Result<policy::ProbeRequestResult, store::StoreError> {
        let now_ms = self.clock.lock().now_ms();
        self.assessments.fetch_add(1, Ordering::SeqCst);
        let source_version = crate::fingerprint_job::current_source_version(&row.file.path);
        let need = {
            let store = self.store.as_ref();
            policy::assess_probe_need(&store, row, origin, now_ms, source_version)
        };
        let allow_when_paused = matches!(
            origin,
            policy::ProbeRequestOrigin::Playback
                | policy::ProbeRequestOrigin::ManualRetry
                | policy::ProbeRequestOrigin::ManualRefresh
        );
        if allow_when_paused
            && matches!(need.retry, policy::ProbeRequestResult::AlreadyRunning)
            && self.fingerprint_dispatch_paused()
        {
            return self.promote_paused_queued_unit(row);
        }
        if !matches!(need.retry, policy::ProbeRequestResult::Complete) {
            return Ok(need.retry);
        }
        if need.metadata_valid && !need.intro_missing && !need.outro_missing {
            return Ok(policy::ProbeRequestResult::Complete);
        }
        let unit = ProbeUnit {
            row: row.clone(),
            kind: need.kind,
            force_fingerprint: need.fingerprint_enabled,
            reuse_fingerprint_cache: true,
            overwrite_markers: false,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        };
        if need.metadata_valid
            && need.fingerprint_enabled
            && (need.intro_missing || need.outro_missing)
        {
            return Ok(
                match self.try_enqueue_direct_fingerprint_inner(
                    unit,
                    need.media_duration_ms,
                    need.source_version,
                    need.sample_duration_secs,
                    allow_when_paused,
                )? {
                    Some(job_id) => policy::ProbeRequestResult::Queued { job_id },
                    None => policy::ProbeRequestResult::AlreadyRunning,
                },
            );
        }
        let ledger_id = unit.row.file.file_id.to_string();
        if origin == policy::ProbeRequestOrigin::BackgroundRetry {
            self.last_failure.lock().remove(&ledger_id);
        } else if !allow_when_paused
            && let Some(next_retry_at_ms) = self.failure_backoff_until_ms(&ledger_id, now_ms)
        {
            return Ok(policy::ProbeRequestResult::Waiting { next_retry_at_ms });
        }
        Ok(match self.try_enqueue_inner(unit, allow_when_paused)? {
            Some(job_id) => policy::ProbeRequestResult::Queued { job_id },
            None => policy::ProbeRequestResult::AlreadyRunning,
        })
    }

    pub fn enqueue_direct_fingerprint(
        &self,
        unit: ProbeUnit,
        media_duration_ms: Option<i64>,
        source_version: String,
        duration_secs: u32,
    ) -> Option<String> {
        match self.try_enqueue_direct_fingerprint_inner(
            unit,
            media_duration_ms,
            source_version,
            duration_secs,
            false,
        ) {
            Ok(job) => job,
            Err(error) => {
                tracing::warn!(%error, "直接声纹请求暂不可入队，保留探测义务等待重试");
                None
            }
        }
    }

    fn try_enqueue_direct_fingerprint_inner(
        &self,
        mut unit: ProbeUnit,
        media_duration_ms: Option<i64>,
        source_version: String,
        duration_secs: u32,
        allow_when_paused: bool,
    ) -> Result<Option<String>, store::StoreError> {
        let ledger_id = unit.row.file.file_id.to_string();
        let job_id = uuid::Uuid::new_v4().to_string();
        // Register the in-memory deduplication id, then release `seen` before the Store lock:
        // admission is the hot path, and a held `seen` block would serialize every worker
        // that finishes a unit behind this one Store write.
        if !self.try_insert_seen(ledger_id.clone(), job_id.clone()) {
            return Ok(None);
        }
        match self.persist_single_unit_with_id(&unit, &job_id, "fingerprint_probe", false) {
            Ok(true) => {}
            Ok(false) => {
                self.remove_seen_if_job(&ledger_id, &job_id);
                return Ok(None);
            }
            Err(error) => {
                self.remove_seen_if_job(&ledger_id, &job_id);
                return Err(error);
            }
        }
        if allow_when_paused {
            self.mark_job_paused_exempt(job_id.clone());
        }
        unit.job_id = Some(job_id.clone());
        let work = probe::FingerprintWork {
            unit: unit.clone(),
            media_duration_ms,
            source_version,
            duration_secs,
            start_job: true,
        };
        let id = store::ProbeDispatchId {
            job_id: job_id.clone(),
            ledger_id: ledger_id.clone(),
            stage: store::ProbeDispatchStage::Fingerprint,
        };
        if (!allow_when_paused && self.fingerprint_dispatch_paused())
            || self.should_defer_remote_dispatch(&unit.row.file.path)
        {
            self.remove_seen_if_job(&ledger_id, &job_id);
            tracing::info!(
                job_id,
                ledger_id,
                "后台探测或远程读取已暂停，直接声纹任务保持数据库排队"
            );
        } else if let Err(err) = self.dispatch_fingerprint_work(id, work) {
            self.remove_seen_if_job(&ledger_id, &job_id);
            tracing::info!(job_id, "声纹窗口已满，直接声纹任务保持数据库排队");
            if !matches!(err, super::dispatch::FingerprintDispatchError::Closed) {
                self.wake.notify_waiters();
            }
        }
        Ok(Some(job_id))
    }

    /// 入队探测（幂等：已在队列/执行中的跳过）。
    ///
    /// 窗口满只限制内存派发：单元已经持久化，保持数据库排队，等窗口空出后由
    /// [`Self::fill_dispatch_windows`] 重新投递。取消单元会把 durable 积压变成永久丢失。
    pub fn enqueue(&self, unit: ProbeUnit) -> bool {
        match self.try_enqueue_inner(unit, false) {
            Ok(job) => job.is_some(),
            Err(error) => {
                tracing::warn!(%error, "媒体信息请求暂不可入队，保留探测义务等待重试");
                false
            }
        }
    }

    fn try_enqueue_inner(
        &self,
        mut unit: ProbeUnit,
        allow_when_paused: bool,
    ) -> Result<Option<String>, store::StoreError> {
        unit.reuse_fingerprint_cache = true;
        unit.reuse_media_info_cache = true;
        let ledger_id = unit.row.file.file_id.to_string();
        if self.has_seen(&ledger_id) {
            return Ok(None);
        }
        if !allow_when_paused
            && self
                .last_failure
                .lock()
                .get(&ledger_id)
                .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
        {
            return Ok(None);
        }
        self.last_failure.lock().remove(&ledger_id);
        let job_id = uuid::Uuid::new_v4().to_string();
        if !self.persist_single_unit_with_id(&unit, &job_id, "media_probe", false)? {
            return Ok(None);
        }
        if allow_when_paused {
            self.mark_job_paused_exempt(job_id.clone());
        }
        unit.job_id = Some(job_id.clone());
        tracing::info!(
            path = %unit.row.file.path.display(),
            job_id,
            "【媒体信息】已进入高优先级探测队列；声纹将在媒体信息写入后另行排队"
        );
        if (!allow_when_paused && self.background_is_paused())
            || self.should_defer_remote_dispatch(&unit.row.file.path)
        {
            tracing::info!(
                ledger_id,
                job_id,
                "后台探测或远程读取已暂停，媒体信息任务保持数据库排队"
            );
        } else if self.try_insert_seen(ledger_id.clone(), job_id.clone())
            && self.queue_metadata(unit).is_err()
        {
            self.remove_seen_if_job(&ledger_id, &job_id);
            tracing::info!(ledger_id, job_id, "媒体信息窗口已满，单元保持数据库排队");
            self.wake.notify_waiters();
        }
        Ok(Some(job_id))
    }

    /// 强制探测：同一文件已有任务时拒绝重复排队；终态后用户可重试。
    pub fn enqueue_force(&self, mut unit: ProbeUnit) -> bool {
        unit.reuse_fingerprint_cache = false;
        unit.reuse_media_info_cache = false;
        let ledger_id = unit.row.file.file_id.to_string();
        let job_id = uuid::Uuid::new_v4().to_string();
        self.last_failure.lock().remove(&ledger_id);
        let paused = self.fingerprint_dispatch_paused();
        // Same hot-path rule as `enqueue`: never hold `seen` across the Store write.
        if !paused {
            if !self.try_insert_seen(ledger_id.clone(), job_id.clone()) {
                tracing::info!(ledger_id, "已有媒体探测任务，跳过重复强制探测");
                return false;
            }
        } else {
            let existing = self.seen.lock().get(&ledger_id).cloned();
            if existing
                .as_deref()
                .is_some_and(|jid| self.is_dispatch_paused_exempt(jid, &ledger_id))
            {
                tracing::info!(ledger_id, "已有豁免媒体探测任务，跳过重复强制探测");
                return false;
            }
        }
        match self.persist_single_unit_with_id(&unit, &job_id, "media_probe", true) {
            Ok(true) => {}
            Ok(false) => {
                self.remove_seen_if_job(&ledger_id, &job_id);
                return false;
            }
            Err(error) => {
                self.remove_seen_if_job(&ledger_id, &job_id);
                tracing::warn!(%error, ledger_id, "强制探测未入队，等待后续请求重试");
                return false;
            }
        }
        unit.job_id = Some(job_id.clone());
        if self.should_defer_remote_dispatch(&unit.row.file.path) {
            self.remove_seen_if_job(&ledger_id, &job_id);
            self.mark_job_paused_exempt(job_id.clone());
            tracing::info!(
                ledger_id,
                job_id,
                "远程读取已暂停，强制探测任务保持数据库排队"
            );
        } else {
            let claimed = !paused || self.try_insert_seen(ledger_id.clone(), job_id.clone());
            self.mark_job_paused_exempt(job_id.clone());
            if claimed && self.queue_metadata(unit).is_err() {
                self.remove_seen_if_job(&ledger_id, &job_id);
                tracing::info!(ledger_id, job_id, "强制探测窗口已满，任务保持数据库排队");
                self.wake.notify_waiters();
            }
        }
        true
    }

    fn promote_paused_queued_unit(
        &self,
        row: &domain::FileCoverage,
    ) -> Result<policy::ProbeRequestResult, store::StoreError> {
        let ledger_id = row.file.file_id.to_string();
        let Some(active) = self.store.active_probe_unit_for_ledger(&ledger_id)? else {
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        };
        if active.status != "queued" && active.status != "running" {
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        }
        let mut already_exempt = self.is_dispatch_paused_exempt(&active.job_id, &ledger_id);
        self.mark_unit_paused_exempt(active.job_id.clone(), ledger_id.clone());
        self.mark_unit_promoted(active.job_id.clone(), ledger_id.clone());
        let Some(current) = self.store.active_probe_unit_for_ledger(&ledger_id)? else {
            self.unmark_unit_paused_exempt(&active.job_id, &ledger_id);
            self.unmark_unit_promoted(&active.job_id, &ledger_id);
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        };
        if current.job_id != active.job_id {
            already_exempt = self.is_dispatch_paused_exempt(&current.job_id, &ledger_id);
            self.unmark_unit_paused_exempt(&active.job_id, &ledger_id);
            self.unmark_unit_promoted(&active.job_id, &ledger_id);
            self.mark_unit_paused_exempt(current.job_id.clone(), ledger_id.clone());
            self.mark_unit_promoted(current.job_id.clone(), ledger_id.clone());
            if !self
                .store
                .is_probe_unit_active(&current.job_id, &ledger_id)?
            {
                self.unmark_unit_paused_exempt(&current.job_id, &ledger_id);
                self.unmark_unit_promoted(&current.job_id, &ledger_id);
                return Ok(policy::ProbeRequestResult::AlreadyRunning);
            }
        }
        if current.status == "running" {
            return Ok(policy::ProbeRequestResult::AlreadyRunning);
        }
        if current.status == "queued" && !self.should_defer_remote_dispatch(&row.file.path) {
            self.dispatch_promoted_unit(&current.job_id, &ledger_id, current.stage);
        }
        if !already_exempt {
            Ok(policy::ProbeRequestResult::Queued {
                job_id: current.job_id,
            })
        } else {
            Ok(policy::ProbeRequestResult::AlreadyRunning)
        }
    }

    pub(super) fn dispatch_promoted_unit(
        &self,
        job_id: &str,
        ledger_id: &str,
        stage: store::ProbeDispatchStage,
    ) {
        match stage {
            store::ProbeDispatchStage::Metadata => {
                if self.try_insert_seen(ledger_id.to_string(), job_id.to_string()) {
                    let id = store::ProbeDispatchId {
                        job_id: job_id.to_string(),
                        ledger_id: ledger_id.to_string(),
                        stage: store::ProbeDispatchStage::Metadata,
                    };
                    if self.dispatch_metadata_id(id).is_err() {
                        self.remove_seen_if_job(ledger_id, job_id);
                        self.wake.notify_waiters();
                    }
                }
            }
            store::ProbeDispatchStage::Fingerprint => {
                if self.fingerprint_overflowed.load(Ordering::Acquire) {
                    self.wake.notify_waiters();
                    return;
                }
                if !self.has_seen(ledger_id) {
                    let id = store::ProbeDispatchId {
                        job_id: job_id.to_string(),
                        ledger_id: ledger_id.to_string(),
                        stage: store::ProbeDispatchStage::Fingerprint,
                    };
                    if let Some((_, work)) = self.load_dispatch_unit(&id) {
                        let inserted =
                            self.try_insert_seen(ledger_id.to_string(), job_id.to_string());
                        if inserted && let Err(err) = self.dispatch_fingerprint_work(id, work) {
                            self.remove_seen_if_job(ledger_id, job_id);
                            if !matches!(err, super::dispatch::FingerprintDispatchError::Closed) {
                                self.wake.notify_waiters();
                            }
                        }
                    }
                }
            }
            store::ProbeDispatchStage::Analysis => {
                self.wake.notify_waiters();
            }
        }
    }

    fn persist_single_unit_with_id(
        &self,
        unit: &ProbeUnit,
        job_id: &str,
        kind: &str,
        supersede_when_paused: bool,
    ) -> Result<bool, store::StoreError> {
        let ledger_id = unit.row.file.file_id.to_string();
        let scope = format!("ledger:{ledger_id}");
        let spec = crate::store::ProbeJobUnitSpec {
            ledger_id: &ledger_id,
            kind: unit.kind.as_str(),
            force_fingerprint: unit.force_fingerprint,
            reuse_fingerprint_cache: unit.reuse_fingerprint_cache,
            overwrite_markers: unit.overwrite_markers,
            reuse_media_info_cache: unit.reuse_media_info_cache,
        };
        // Presence may have changed after policy assessment. Keep this final check and
        // persistence under the same Store lock that presence writes take: a source
        // recorded missing must never gain probe work, whichever admission path asked.
        let (created, cancelled) = self.store.library_write(|store| {
            if store.ledger_is_missing(&ledger_id)? {
                tracing::info!(ledger_id, "源文件已标记缺失，拒绝创建探测任务");
                return Ok((false, Vec::new()));
            }
            let paused = supersede_when_paused && self.fingerprint_dispatch_paused();
            if !paused {
                let created = store.create_probe_job(
                    job_id,
                    kind,
                    &unit.row.file.media_id.to_string(),
                    single_season_opt(&unit.row),
                    &scope,
                    &[spec],
                )?;
                return Ok((created, Vec::new()));
            }
            let include_metadata = self.background_is_paused();
            let exempt_jobs = self.paused_exempt_jobs_for_ledgers(std::slice::from_ref(&ledger_id));
            enum SingleTxErr {
                Conflict,
                Store(store::StoreError),
            }
            match store.with_write_tx(SingleTxErr::Store, |store| {
                let cancelled = store
                    .cancel_dormant_queued_units_for_ledgers_scoped(
                        std::slice::from_ref(&ledger_id),
                        &exempt_jobs,
                        include_metadata,
                        "superseded_by_manual_probe",
                    )
                    .map_err(SingleTxErr::Store)?;
                let created = store
                    .create_probe_job(
                        job_id,
                        kind,
                        &unit.row.file.media_id.to_string(),
                        single_season_opt(&unit.row),
                        &scope,
                        &[spec],
                    )
                    .map_err(SingleTxErr::Store)?;
                if !created {
                    return Err(SingleTxErr::Conflict);
                }
                Ok(cancelled)
            }) {
                Ok(cancelled) => Ok((true, cancelled)),
                Err(SingleTxErr::Conflict) => Ok((false, Vec::new())),
                Err(SingleTxErr::Store(err)) => Err(err),
            }
        })?;
        if !created {
            return Ok(false);
        }
        self.finish_superseding_cancelled_units(&cancelled);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Media, MediaId, MediaKind};
    use std::sync::Arc;

    fn seed_tv_row(store: &crate::Store, root: &std::path::Path, ep: u32) -> domain::FileCoverage {
        let path = root.join(format!("S01E{ep:02}.mkv"));
        std::fs::write(&path, b"fixture").unwrap();
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: format!("Ep {ep}"),
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
        let row = super::super::test_support::publish(
            store,
            &super::super::test_support::coverage(
                media_id,
                path.to_str().unwrap(),
                Some(1),
                Some(ep),
            ),
        );
        let actual_version = crate::fingerprint_job::current_source_version(&row.file.path);
        store
            .put_file_meta_versioned(
                &row.file.file_id.to_string(),
                &library::Tracks {
                    video: Some(library::VideoTrack::default()),
                    ..Default::default()
                },
                Some(&actual_version),
                Some(1_200_000),
            )
            .unwrap();
        row
    }

    #[test]
    fn enqueue_force_when_paused_supersedes_dormant_unit_even_with_stale_seen_and_fingerprint_overflow_blocks_jumping()
     {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let store = Arc::new(crate::Store::open(base.join("data")).unwrap());
        let root = base.join("tv");
        std::fs::create_dir_all(&root).unwrap();
        let lib = store
            .create_library(
                MediaKind::Tv,
                "TV",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .set_library_intro_settings(&lib.id, true, true)
            .unwrap();

        let rows: Vec<_> = (1..=18).map(|ep| seed_tv_row(&store, &root, ep)).collect();
        let manager = ProbeManager::new(store.clone()).unwrap();
        for row in &rows[..17] {
            assert!(matches!(
                manager
                    .request_probe(row, policy::ProbeRequestOrigin::Detail)
                    .unwrap(),
                policy::ProbeRequestResult::Queued { .. }
            ));
        }
        // Drain 1 slot from fingerprint_rx while row[16] remains in SQLite backlog.
        assert!(manager.take_fingerprint_for_test().is_some());
        // Live arrival row[17] must NOT jump into the freed slot ahead of row[16]!
        assert!(matches!(
            manager
                .request_probe(&rows[17], policy::ProbeRequestOrigin::Detail)
                .unwrap(),
            policy::ProbeRequestResult::Queued { .. }
        ));
        assert!(
            !manager.holds_dispatch_id(&rows[17].file.file_id.to_string()),
            "direct fingerprint admission must set fingerprint_overflowed and prevent line-jumping"
        );

        // Simulate pause racing with seen still holding a dormant non-exempt job_id on rows[1].
        manager.set_background_paused_flag(true);
        assert!(manager.holds_dispatch_id(&rows[1].file.file_id.to_string()));
        assert!(
            manager.enqueue_force(ProbeUnit {
                row: rows[1].clone(),
                kind: MediaKind::Tv,
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: false,
                reuse_media_info_cache: false,
                marker_refresh_id: None,
                job_id: None,
            }),
            "enqueue_force while paused must supersede dormant non-exempt unit even if seen still holds it"
        );
    }
}
