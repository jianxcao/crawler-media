//! Standalone fingerprint queue pause/resume, concurrency admission, and in-flight abort.
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

    pub(super) fn fingerprint_dispatch_paused(&self) -> bool {
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
    ) -> usize {
        if !scope.include_running {
            return 0;
        }
        let controls: Vec<marker::WorkControl> = {
            let guard = self.active_fingerprint_controls.lock();
            guard
                .values()
                .filter(|entry| {
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
                .map(|entry| entry.control.clone())
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
        for job_id in &summary.cancelled_job_ids {
            if let Ok(Some(job)) = self.store.get_probe_job(job_id)
                && !job.is_active()
            {
                self.unmark_job_paused_exempt(job_id);
                super::timings::finalize_terminal_job(&self.store_access(), &self.timings, &job);
            }
        }
        self.purge_cancelled_seen(&summary.cancelled_file_ids, false);
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
        let paused = self.fingerprint_dispatch_paused();
        let exempt_jobs = if paused {
            self.paused_exempt_jobs_for_ledgers(std::slice::from_ref(&ledger_id))
        } else {
            std::collections::HashSet::new()
        };
        let scope = format!("ledger:{ledger_id}");
        let spec = store::ProbeJobUnitSpec {
            ledger_id: &ledger_id,
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
                    .ledger_is_missing(&ledger_id)
                    .map_err(CreateErr::Store)?
                {
                    return Err(CreateErr::Conflict);
                }
                store.with_write_tx(CreateErr::Store, |store| {
                    let cancelled = if paused {
                        store
                            .cancel_dormant_queued_units_for_ledgers(
                                std::slice::from_ref(&ledger_id),
                                &exempt_jobs,
                                "superseded_by_manual_probe",
                            )
                            .map_err(CreateErr::Store)?
                    } else {
                        Vec::new()
                    };
                    let created = store
                        .create_probe_job(
                            &job_id,
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
        let cancelled = match outcome {
            Ok(cancelled) => cancelled,
            Err(CreateErr::Conflict) => return Ok((false, None)),
            Err(CreateErr::Store(err)) => return Err(err),
        };
        self.finish_superseding_cancelled_units(&cancelled);
        self.mark_job_paused_exempt(job_id.clone());
        unit.job_id = Some(job_id.clone());

        if !self.should_defer_remote_dispatch(&unit.row.file.path)
            && self.try_insert_seen(ledger_id.clone(), job_id.clone())
        {
            if job_kind == "fingerprint_probe" {
                let id = store::ProbeDispatchId {
                    job_id: job_id.clone(),
                    ledger_id: ledger_id.clone(),
                    stage: store::ProbeDispatchStage::Fingerprint,
                };
                let work = probe::FingerprintWork {
                    unit,
                    media_duration_ms: cached_meta.and_then(|v| v.format_duration_ms),
                    source_version: actual_version,
                    duration_secs: sample_duration_secs,
                    start_job: true,
                };
                if self.dispatch_fingerprint_work(id, work).is_err() {
                    self.remove_seen_if_job(&ledger_id, &job_id);
                    self.wake.notify_waiters();
                }
            } else if self.queue_metadata(unit).is_err() {
                self.remove_seen_if_job(&ledger_id, &job_id);
                self.wake.notify_waiters();
            }
        }
        Ok((true, Some(job_id)))
    }
}
