use domain::Subscribe;

use crate::management::ApiState;

use super::decision::ensure_current;
use super::plan::{TransferPlan, with_plan_write};

pub(crate) struct PublicationRecorder<'a> {
    pub state: &'a ApiState,
    pub subscribe: &'a Subscribe,
    pub lease: &'a crate::library_lifecycle::LibraryWorkLease,
    pub fingerprint: store::ReadFingerprint,
    pub snapshot: store::SubscribeLibrarySnapshot,
}

impl PublicationRecorder<'_> {
    fn prepare_with_plan(
        &self,
        candidate: &domain::FileCoverage,
        source: &std::path::Path,
        mode: library::TransferMode,
        plan: Option<&TransferPlan>,
        ignored_files: &std::collections::HashSet<String>,
    ) -> Result<domain::FileCoverage, subscribe::SubscribeError> {
        let result = (|| -> Result<domain::FileCoverage, String> {
            let guard = self.state.subscribe_guard(self.subscribe.id);
            let _guard = guard.lock();
            let store = self.state.store.as_ref();
            ensure_current(self.state, store, self.subscribe)?;
            if store
                .load_subscribe_facts(self.subscribe.id)
                .map_err(|error| error.to_string())?
                != self.snapshot.saved
            {
                return Err("Transfer Subscribe facts changed".into());
            }
            if candidate.file.media_id != self.subscribe.media_id
                || candidate.file.library_id.to_string() != self.lease.library_id()
                || candidate.file.source_path.as_deref() != Some(source)
                || candidate.file.transfer_mode.as_deref() != Some(mode.as_str())
            {
                return Err(
                    "Transfer candidate Media, Library, source or mode conflicts with its lease"
                        .into(),
                );
            }
            // The reservation owns the pending row identity: an interrupted round
            // that left the same destination behind is recovered here rather than
            // re-created, and a conflicting owner/source/mode is refused.
            self.publication_write(plan, Some(&self.fingerprint), ignored_files, |db| {
                super::snapshot::validate_library_snapshot(db, &self.snapshot, ignored_files)?;
                let config_revision = match plan {
                    Some(plan) => plan.config_revision,
                    None => db
                        .library_config_revision(self.lease.library_id())
                        .map_err(|error| error.to_string())?,
                };
                let scope = store::library_write::WriteScope {
                    library_id: candidate.file.library_id,
                    config_revision,
                };
                db.reserve_transfer(
                    &scope,
                    candidate,
                    source,
                    mode,
                    crate::job_loop::unix_now() * 1000,
                )
                .map_err(|error| error.to_string())
            })
        })();
        result.map_err(|error| {
            tracing::error!(path = %candidate.file.path.display(), %error, "Transfer publication reservation failed");
            subscribe::SubscribeError::Downloader(downloader::DownloaderError::Message(error))
        })
    }

    /// Complete the publication of the reserved file. The reservation and this
    /// call carry the same `file_id`, so the row keeps its root, source path and
    /// transfer mode; the completion is a version-checked publish, not a second
    /// identity creation.
    fn published_with_plan(
        &self,
        candidate: &domain::FileCoverage,
        plan: Option<&TransferPlan>,
    ) -> Result<(), subscribe::SubscribeError> {
        let fail = |error: String| {
            tracing::error!(path = %candidate.file.path.display(), %error, "Transfer publication completion failed");
            subscribe::SubscribeError::Downloader(downloader::DownloaderError::Message(error))
        };
        // Capture the just-placed bytes before the Store guard. Vetoed arrivals
        // are absent from RunOutcome.files, so their deletion authority must be
        // retained by the recorder that actually published this reservation.
        let source = library::observe_source(&candidate.file.path)
            .map_err(|error| fail(error.to_string()))?;
        let rel_path = crate::plugins::events::compute_relative_path(
            self.state.store.as_ref(),
            candidate.file.library_id,
            &candidate.file.path,
        );
        let now_ms = crate::job_loop::unix_now().saturating_mul(1_000);
        let outbox = crate::plugins::events::build_transfer_completed_event(
            candidate.file.media_id,
            candidate.file.library_id,
            Some(self.subscribe.id),
            rel_path,
            1,
            source.size_bytes.unwrap_or(0),
            format!("transfer:subscribe:{}", self.subscribe.id),
            now_ms,
        );
        {
            let guard = self.state.subscribe_guard(self.subscribe.id);
            let _guard = guard.lock();
            let store = self.state.store.as_ref();
            ensure_current(self.state, store, self.subscribe).map_err(fail)?;
            self.publication_write(plan, None, &std::collections::HashSet::new(), |db| {
                let config_revision = match plan {
                    Some(plan) => plan.config_revision,
                    None => db
                        .library_config_revision(self.lease.library_id())
                        .map_err(|error| error.to_string())?,
                };
                let scope = store::library_write::WriteScope {
                    library_id: candidate.file.library_id,
                    config_revision,
                };
                db.finish_transfer_with_outbox(
                    &scope,
                    candidate,
                    &candidate.file.version,
                    now_ms,
                    outbox.as_ref(),
                )
                .map_err(|error| error.to_string())
            })
            .map_err(fail)
        }?;
        if outbox.is_some() {
            crate::plugins::events::wake(self.state);
        }
        if let Some(plan) = plan {
            plan.published_removals.lock().insert(
                candidate.file.path.clone(),
                super::removals::TransferRemovalPin::published(candidate, &source),
            );
        }
        Ok(())
    }
    fn publication_write<T>(
        &self,
        plan: Option<&TransferPlan>,
        fingerprint: Option<&store::ReadFingerprint>,
        ignored: &std::collections::HashSet<String>,
        work: impl FnOnce(&store::LibraryConnection<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let store = self.state.store.as_ref();
        if let Some(plan) = plan {
            return with_plan_write(store, self.lease, fingerprint, ignored, plan, work);
        }
        store.library_write_with_error(
            |error| error.to_string(),
            |db| {
                self.lease
                    .control()
                    .check()
                    .map_err(|error| error.to_string())?;
                let fingerprint_changed = match fingerprint {
                    Some(expected) => {
                        db.read_fingerprint(self.lease.library_id(), self.subscribe.media_id)
                            .map_err(|error| error.to_string())?
                            != *expected
                    }
                    None => false,
                };
                if !db
                    .library_accepts_work(self.lease.library_id())
                    .map_err(|error| error.to_string())?
                    || fingerprint_changed
                {
                    return Err("Transfer source or Library configuration changed".into());
                }
                work(db)
            },
        )
    }
}

pub(crate) struct PlannedPublicationRecorder<'a> {
    pub(crate) recorder: PublicationRecorder<'a>,
    pub(crate) plan: &'a TransferPlan,
    /// File rows this round has already reserved/published. Their appearance in
    /// the scoped fingerprint is this round's own effect, not an external change.
    reserved: std::sync::Mutex<std::collections::HashSet<String>>,
    /// One sequential placement attempt owns one path. A failed attempt is
    /// released before choosing another destination; completion/drop releases it.
    active_path: std::sync::Mutex<Option<crate::http::file_delete::PathClaimGuard>>,
}

impl<'a> PlannedPublicationRecorder<'a> {
    pub(crate) fn new(
        recorder: PublicationRecorder<'a>,
        plan: &'a TransferPlan,
    ) -> PlannedPublicationRecorder<'a> {
        PlannedPublicationRecorder {
            recorder,
            plan,
            reserved: std::sync::Mutex::new(std::collections::HashSet::new()),
            active_path: std::sync::Mutex::new(None),
        }
    }
}

impl subscribe::TransferRecorder for PlannedPublicationRecorder<'_> {
    fn prepare(
        &self,
        candidate: &domain::FileCoverage,
        source: &std::path::Path,
        mode: library::TransferMode,
    ) -> Result<domain::FileCoverage, subscribe::SubscribeError> {
        // Retire a failed placement before an alternate-name attempt. No mutex
        // guard is retained while waiting, observing bytes or writing to Store.
        drop(
            self.active_path
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take(),
        );
        let key = self
            .recorder
            .state
            .store
            .recorded_path_key(&candidate.file.path.to_string_lossy());
        let claim = crate::http::file_delete::transfer_claim(&key).lock();
        let mut reserved = self.reserved.lock().map_err(|_| {
            subscribe::SubscribeError::Downloader(downloader::DownloaderError::Message(
                "Transfer publication recorder poisoned".into(),
            ))
        })?;
        let result =
            self.recorder
                .prepare_with_plan(candidate, source, mode, Some(self.plan), &reserved);
        // The returned coverage is the authoritative reservation: a restarted
        // round recovering a durable pending row gets that row's identity back,
        // not the fresh candidate UUID, so that is the id this round wrote.
        if let Ok(reservation) = &result {
            reserved.insert(reservation.file.file_id.to_string());
            *self.active_path.lock().unwrap_or_else(|p| p.into_inner()) = Some(claim);
        }
        result
    }

    fn published(&self, candidate: &domain::FileCoverage) -> Result<(), subscribe::SubscribeError> {
        let _claim = self
            .active_path
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.recorder
            .published_with_plan(candidate, Some(self.plan))
    }
}
