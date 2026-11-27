//! Standalone fingerprint queue pause/resume, concurrency admission, and in-flight abort.
use std::collections::HashSet;
use std::sync::atomic::Ordering;

use crate::scrape_store::ScrapeStoreExt;

use super::{ProbeManager, ProbeUnit, probe, single_season_opt};

pub(super) struct ActiveFingerprintControl {
    pub(super) library_id: String,
    pub(super) media_id: String,
    pub(super) season: Option<u32>,
    pub(super) control: marker::WorkControl,
}

impl ProbeManager {
    pub fn fingerprint_is_paused(&self) -> bool {
        self.fingerprint_paused.load(Ordering::Acquire)
    }

    pub(crate) fn fingerprint_dispatch_paused(&self) -> bool {
        self.background_is_paused() || self.fingerprint_is_paused()
    }

    pub(crate) fn fingerprint_worker_limit(&self) -> usize {
        self.fingerprint_settings.lock().workers
    }

    pub(crate) fn active_fingerprint_workers(&self) -> usize {
        self.fingerprint_admission.lock().active
    }

    pub(crate) fn apply_fingerprint_settings(
        &self,
        settings: crate::probe_settings::FingerprintProbeSettings,
    ) {
        let _publish = self.settings_publish_lock.lock();
        self.apply_fingerprint_settings_locked(settings);
        self.settings_revision.fetch_add(1, Ordering::Release);
    }

    pub(super) fn apply_fingerprint_settings_locked(
        &self,
        settings: crate::probe_settings::FingerprintProbeSettings,
    ) {
        let was_paused = self
            .fingerprint_paused
            .swap(settings.paused, Ordering::AcqRel);
        *self.fingerprint_settings.lock() = settings;
        self.fingerprint_admission.lock().limit = settings.workers;
        self.fingerprint_capacity.notify_waiters();
        if settings.paused {
            if !was_paused {
                self.analysis_schedule.lock().clear_unstarted();
            }
            self.drain_fingerprint_dispatch();
            if self.has_paused_exempt_work() {
                self.wake.notify_waiters();
            }
        } else {
            if !self.background_is_paused() {
                self.fill_fingerprint_window_now();
            }
            self.wake.notify_waiters();
        }
    }

    pub(super) fn drain_fingerprint_dispatch(&self) {
        self.wake.notify_waiters();
        let drained = {
            let mut guard = self.fingerprint_rx.lock();
            let mut ids = Vec::new();
            if let Some(rx) = guard.as_mut() {
                while let Ok((id, _)) = rx.try_recv() {
                    ids.push(id);
                }
            }
            ids
        };
        if !drained.is_empty() {
            let mut seen = self.seen.lock();
            for id in &drained {
                super::remove_seen_entry_if_job(&mut seen, &id.ledger_id, &id.job_id);
            }
        }
        self.fingerprint_overflowed.store(false, Ordering::Release);
        self.wake.notify_waiters();
    }

    pub(super) fn register_active_fingerprint(
        &self,
        job_id: &str,
        ledger_id: &str,
        unit: &ProbeUnit,
        control: marker::WorkControl,
    ) {
        self.active_fingerprint_controls.lock().insert(
            (job_id.to_string(), ledger_id.to_string()),
            ActiveFingerprintControl {
                library_id: unit.row.file.library_id.to_string(),
                media_id: unit.row.file.media_id.to_string(),
                season: single_season_opt(&unit.row),
                control,
            },
        );
    }

    pub(super) fn unregister_active_fingerprint(&self, job_id: &str, ledger_id: &str) {
        self.active_fingerprint_controls
            .lock()
            .remove(&(job_id.to_string(), ledger_id.to_string()));
    }

    pub(crate) fn abort_matching_active_fingerprints(
        &self,
        scope: &store::FingerprintCancelScope,
        cancelled_file_ids: &[String],
    ) -> usize {
        let extra_files: HashSet<&str> = cancelled_file_ids.iter().map(String::as_str).collect();
        if !scope.include_running {
            return 0;
        }
        let controls: Vec<marker::WorkControl> = {
            let guard = self.active_fingerprint_controls.lock();
            guard
                .iter()
                .filter(|((_, ledger_id), entry)| {
                    if extra_files.contains(ledger_id.as_str()) {
                        return true;
                    }
                    if let Some(fid) = scope.file_id.as_deref()
                        && ledger_id != fid
                    {
                        return false;
                    }
                    if let Some(lib) = scope.library_id.as_deref()
                        && entry.library_id != lib
                    {
                        return false;
                    }
                    if let Some(media) = scope.media_id.as_deref()
                        && entry.media_id != media
                    {
                        return false;
                    }
                    if let Some(season) = scope.season
                        && entry.season != Some(season)
                    {
                        return false;
                    }
                    true
                })
                .map(|(_, entry)| entry.control.clone())
                .collect()
        };
        for control in &controls {
            control.cancel();
        }
        controls.len()
    }

    pub(crate) fn purge_cancelled_fingerprint(
        &self,
        summary: &store::FingerprintCancelSummary,
        global_fingerprint_cancel: bool,
    ) {
        if global_fingerprint_cancel {
            self.analysis_schedule.lock().clear_unstarted();
            self.drain_fingerprint_dispatch();
        }
        self.purge_cancelled_seen(&summary.cancelled_file_ids, false);
        for job_id in &summary.cancelled_job_ids {
            if let Ok(Some(job)) = self.store.get_probe_job(job_id) {
                if !job.is_active() {
                    self.unmark_job_paused_exempt(job_id);
                    super::timings::finalize_terminal_job(
                        &self.store_access(),
                        &self.timings,
                        &job,
                    );
                } else if job.kind == "marker_refresh" {
                    let _ = self.reconcile_marker_refresh_after_retirement(job_id);
                }
            }
        }
    }

    /// Queue an explicit pause-exempt marker detection for a single episode file.
    pub(crate) fn enqueue_single_episode_marker_detect(
        &self,
        mut unit: ProbeUnit,
        reuse_cache: bool,
    ) -> Result<(bool, Option<String>), store::StoreError> {
        let ledger_id = unit.row.file.file_id.to_string();
        unit.force_fingerprint = true;
        unit.reuse_fingerprint_cache = reuse_cache;
        unit.overwrite_markers = true;
        unit.reuse_media_info_cache = true;
        self.last_failure.lock().remove(&ledger_id);

        let actual_version = crate::fingerprint_job::current_source_version(&unit.row.file.path);
        let cached_meta = self
            .store
            .get_media_info_cache_version(&ledger_id)?
            .filter(|v| {
                v.source_version == actual_version
                    && v.source_version == unit.row.file.version.source_version
            });
        let sample_duration_secs = self
            .store
            .get_scrape_config()?
            .effective
            .fingerprint_duration_secs;
        let job_kind = if cached_meta.is_some() {
            "fingerprint_probe"
        } else {
            "media_probe"
        };
        let job_id = uuid::Uuid::new_v4().to_string();
        let Some(cancelled) = self.persist_single_episode_marker_job(
            &unit,
            &ledger_id,
            &job_id,
            job_kind,
            reuse_cache,
        )?
        else {
            return Ok((false, None));
        };
        self.finish_superseding_cancelled_units(&cancelled);
        self.mark_job_paused_exempt(job_id.clone());
        unit.job_id = Some(job_id.clone());
        self.dispatch_single_episode_marker_job(
            unit,
            &ledger_id,
            &job_id,
            job_kind,
            cached_meta.and_then(|v| v.format_duration_ms),
            actual_version,
            sample_duration_secs,
        );
        Ok((true, Some(job_id)))
    }

    fn persist_single_episode_marker_job(
        &self,
        unit: &ProbeUnit,
        ledger_id: &str,
        job_id: &str,
        job_kind: &str,
        reuse_cache: bool,
    ) -> Result<Option<Vec<(String, String)>>, store::StoreError> {
        let paused = self.fingerprint_dispatch_paused();
        let include_metadata = self.background_is_paused();
        let lid_owned = ledger_id.to_string();
        let exempt_jobs = if paused {
            self.paused_exempt_jobs_for_ledgers(std::slice::from_ref(&lid_owned))
        } else {
            HashSet::new()
        };
        let scope = format!("ledger:{ledger_id}");
        let spec = store::ProbeJobUnitSpec {
            ledger_id,
            kind: unit.kind.as_str(),
            force_fingerprint: true,
            reuse_fingerprint_cache: reuse_cache,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        };
        enum CreateErr {
            Conflict,
            Store(store::StoreError),
        }
        let outcome = self
            .store
            .library_write_with_error(CreateErr::Store, |store| {
                if store
                    .ledger_is_missing(ledger_id)
                    .map_err(CreateErr::Store)?
                {
                    return Err(CreateErr::Conflict);
                }
                store.with_write_tx(CreateErr::Store, |store| {
                    let cancelled = if paused {
                        store
                            .cancel_dormant_queued_units_for_ledgers_scoped(
                                std::slice::from_ref(&lid_owned),
                                &exempt_jobs,
                                include_metadata,
                                "superseded_by_manual_probe",
                            )
                            .map_err(CreateErr::Store)?
                    } else {
                        Vec::new()
                    };
                    let created = store
                        .create_probe_job(
                            job_id,
                            job_kind,
                            &unit.row.file.media_id.to_string(),
                            single_season_opt(&unit.row),
                            &scope,
                            &[spec],
                        )
                        .map_err(CreateErr::Store)?;
                    if !created {
                        return Err(CreateErr::Conflict);
                    }
                    Ok(cancelled)
                })
            });
        match outcome {
            Ok(cancelled) => Ok(Some(cancelled)),
            Err(CreateErr::Conflict) => Ok(None),
            Err(CreateErr::Store(err)) => Err(err),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_single_episode_marker_job(
        &self,
        unit: ProbeUnit,
        ledger_id: &str,
        job_id: &str,
        job_kind: &str,
        media_duration_ms: Option<i64>,
        source_version: String,
        duration_secs: u32,
    ) {
        if self.should_defer_remote_dispatch(&unit.row.file.path)
            || !self.try_insert_seen(ledger_id.to_string(), job_id.to_string())
        {
            return;
        }
        if job_kind == "fingerprint_probe" {
            let id = store::ProbeDispatchId {
                job_id: job_id.to_string(),
                ledger_id: ledger_id.to_string(),
                stage: store::ProbeDispatchStage::Fingerprint,
            };
            let work = probe::FingerprintWork {
                unit,
                media_duration_ms,
                source_version,
                duration_secs,
                start_job: true,
            };
            if self.dispatch_fingerprint_work(id, work).is_err() {
                self.remove_seen_if_job(ledger_id, job_id);
                self.wake.notify_waiters();
            }
        } else if self.queue_metadata(unit).is_err() {
            self.remove_seen_if_job(ledger_id, job_id);
            self.wake.notify_waiters();
        }
    }
}
