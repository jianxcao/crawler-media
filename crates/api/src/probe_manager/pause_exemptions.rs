//! In-memory pause exemption and promotion tracking for `ProbeManager`.

use std::collections::{BTreeSet, HashSet};

use super::ProbeManager;

impl ProbeManager {
    pub(super) fn mark_job_paused_exempt(&self, job_id: String) -> bool {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id)
    }

    pub(super) fn unmark_job_paused_exempt(&self, job_id: &str) {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(job_id);
        self.paused_exempt_units.lock().retain(|(j, _)| j != job_id);
        self.promoted_units.lock().retain(|(j, _)| j != job_id);
    }

    pub(super) fn is_job_paused_exempt(&self, job_id: &str) -> bool {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(job_id)
    }

    pub(super) fn mark_unit_paused_exempt(&self, job_id: String, ledger_id: String) -> bool {
        self.paused_exempt_units.lock().insert((job_id, ledger_id))
    }

    pub(super) fn unmark_unit_paused_exempt(&self, job_id: &str, ledger_id: &str) {
        self.paused_exempt_units
            .lock()
            .retain(|(j, l)| !(j == job_id && l == ledger_id));
    }

    pub(super) fn is_unit_paused_exempt(&self, job_id: &str, ledger_id: &str) -> bool {
        self.paused_exempt_units
            .lock()
            .iter()
            .any(|(j, l)| j == job_id && l == ledger_id)
    }

    pub(super) fn is_dispatch_paused_exempt(&self, job_id: &str, ledger_id: &str) -> bool {
        self.is_job_paused_exempt(job_id) || self.is_unit_paused_exempt(job_id, ledger_id)
    }

    pub(super) fn job_has_unit_exemption_or_promotion(&self, job_id: &str) -> bool {
        let exempt = self
            .paused_exempt_units
            .lock()
            .iter()
            .any(|(j, _)| j == job_id);
        exempt || self.promoted_units.lock().iter().any(|(j, _)| j == job_id)
    }

    pub(super) fn has_paused_exempt_jobs(&self) -> bool {
        !self
            .paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    pub fn has_paused_exempt_work(&self) -> bool {
        self.has_paused_exempt_jobs() || !self.paused_exempt_units.lock().is_empty()
    }

    pub(super) fn paused_exempt_job_ids(&self) -> Vec<String> {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    pub(super) fn paused_exempt_unit_pairs(&self) -> Vec<(String, String)> {
        self.paused_exempt_units.lock().iter().cloned().collect()
    }

    pub(super) fn paused_exempt_jobs_for_ledgers(&self, ledger_ids: &[String]) -> HashSet<String> {
        let mut jobs = self
            .paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for (job_id, lid) in self.paused_exempt_units.lock().iter() {
            if ledger_ids.iter().any(|target| target == lid) {
                jobs.insert(job_id.clone());
            }
        }
        for (job_id, lid) in self.promoted_units.lock().iter() {
            if ledger_ids.iter().any(|target| target == lid) {
                jobs.insert(job_id.clone());
            }
        }
        jobs
    }

    pub(super) fn prune_inactive_exempt_jobs(&self, checked: &[String], active: &[String]) {
        let active_set: HashSet<&str> = active.iter().map(String::as_str).collect();
        let mut guard = self
            .paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for job_id in checked {
            if !active_set.contains(job_id.as_str()) {
                guard.remove(job_id);
            }
        }
    }

    pub(super) fn prune_inactive_exempt_units(
        &self,
        checked: &[(String, String)],
        active: &[(String, String)],
    ) {
        let active_set: HashSet<&(String, String)> = active.iter().collect();
        let mut pruned_jobs = BTreeSet::new();
        {
            let mut guard = self.paused_exempt_units.lock();
            for pair in checked {
                if !active_set.contains(pair) && guard.remove(pair) {
                    pruned_jobs.insert(pair.0.clone());
                }
            }
        }
        if pruned_jobs.is_empty() {
            return;
        }
        {
            let mut promoted = self.promoted_units.lock();
            for pair in checked {
                if !active_set.contains(pair) {
                    promoted.remove(pair);
                }
            }
        }
        for job_id in pruned_jobs {
            if self.job_has_unit_exemption_or_promotion(&job_id)
                && self
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

    pub(super) fn mark_unit_promoted(&self, job_id: String, ledger_id: String) -> bool {
        self.promoted_units.lock().insert((job_id, ledger_id))
    }

    pub(super) fn unmark_unit_promoted(&self, job_id: &str, ledger_id: &str) {
        self.promoted_units
            .lock()
            .retain(|(j, l)| !(j == job_id && l == ledger_id));
    }

    pub(super) fn was_unit_promoted(&self, job_id: &str, ledger_id: &str) -> bool {
        self.promoted_units
            .lock()
            .iter()
            .any(|(j, l)| j == job_id && l == ledger_id)
    }

    pub(super) fn promoted_unit_pairs(&self) -> Vec<(String, String)> {
        self.promoted_units.lock().iter().cloned().collect()
    }

    pub(super) fn prune_inactive_promoted_units(
        &self,
        checked: &[(String, String)],
        active: &[(String, String)],
    ) {
        let active_set: HashSet<&(String, String)> = active.iter().collect();
        let mut guard = self.promoted_units.lock();
        for pair in checked {
            if !active_set.contains(pair) {
                guard.remove(pair);
            }
        }
    }

    pub(super) fn clear_paused_exempt_jobs(&self) {
        self.paused_exempt_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.paused_exempt_units.lock().clear();
        self.promoted_units.lock().clear();
    }

    pub(super) fn finish_superseding_cancelled_units(&self, cancelled: &[(String, String)]) {
        if cancelled.is_empty() {
            return;
        }
        {
            let mut seen = self.seen.lock();
            for (cancelled_job_id, file_id) in cancelled {
                super::remove_seen_entry_if_job(&mut seen, file_id, cancelled_job_id);
            }
        }
        for (cancelled_job_id, file_id) in cancelled {
            self.unmark_unit_paused_exempt(cancelled_job_id, file_id);
            self.unmark_unit_promoted(cancelled_job_id, file_id);
        }
        let unique_jobs: BTreeSet<&str> = cancelled.iter().map(|(jid, _)| jid.as_str()).collect();
        for cancelled_job_id in unique_jobs {
            match self.store.get_probe_job(cancelled_job_id) {
                Ok(Some(job)) if !job.is_active() => {
                    self.unmark_job_paused_exempt(&job.id);
                }
                Ok(Some(job)) if job.kind == "marker_refresh" => {
                    let _ = self.reconcile_marker_refresh_after_retirement(cancelled_job_id);
                }
                Ok(None) => {
                    self.unmark_job_paused_exempt(cancelled_job_id);
                }
                Ok(Some(_)) => {}
                Err(_) => {
                    let _ = self.reconcile_marker_refresh_after_retirement(cancelled_job_id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use domain::{LedgerId, MediaId};

    use super::super::ProbeManager;

    #[test]
    fn cancel_deleted_dispatch_cancels_all_done_job_and_clears_exemptions_and_promotions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
        let manager = ProbeManager::new_paused(store.clone()).unwrap();
        let missing_ledger = LedgerId::new().to_string();
        let media_id = MediaId::new().to_string();
        assert!(
            store
                .create_probe_job(
                    "orphan-job",
                    "media_probe",
                    &media_id,
                    None,
                    &format!("ledger:{missing_ledger}"),
                    &[store::ProbeJobUnitSpec {
                        ledger_id: &missing_ledger,
                        kind: "movie",
                        force_fingerprint: false,
                        reuse_fingerprint_cache: true,
                        overwrite_markers: false,
                        reuse_media_info_cache: true,
                    }],
                )
                .unwrap()
        );
        manager.mark_job_paused_exempt("orphan-job".into());
        manager.mark_unit_paused_exempt("orphan-job".into(), missing_ledger.clone());
        manager.mark_unit_promoted("orphan-job".into(), missing_ledger.clone());
        assert!(manager.try_insert_seen(missing_ledger.clone(), "orphan-job".into()));

        super::super::worker::cancel_deleted_dispatch(
            &manager,
            &store::ProbeDispatchId {
                job_id: "orphan-job".into(),
                ledger_id: missing_ledger.clone(),
                stage: store::ProbeDispatchStage::Metadata,
            },
        );

        assert_eq!(
            store.get_probe_job("orphan-job").unwrap().unwrap().status,
            "cancelled"
        );
        assert!(!manager.has_paused_exempt_work());
        assert!(!manager.job_has_unit_exemption_or_promotion("orphan-job"));
        assert!(!manager.holds_dispatch_id(&missing_ledger));

        // Also cleans up when orphan-media purge already deleted the probe_jobs row itself.
        let purged_ledger = LedgerId::new().to_string();
        manager.mark_job_paused_exempt("purged-job".into());
        manager.mark_unit_paused_exempt("purged-job".into(), purged_ledger.clone());
        manager.mark_unit_promoted("purged-job".into(), purged_ledger.clone());
        assert!(manager.try_insert_seen(purged_ledger.clone(), "purged-job".into()));

        super::super::worker::cancel_deleted_dispatch(
            &manager,
            &store::ProbeDispatchId {
                job_id: "purged-job".into(),
                ledger_id: purged_ledger.clone(),
                stage: store::ProbeDispatchStage::Metadata,
            },
        );
        assert!(!manager.has_paused_exempt_work());
        assert!(!manager.job_has_unit_exemption_or_promotion("purged-job"));
        assert!(!manager.holds_dispatch_id(&purged_ledger));
    }
}
