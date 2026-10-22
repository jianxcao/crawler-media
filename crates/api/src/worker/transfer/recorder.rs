use domain::Subscribe;

use crate::management::ApiState;

use super::plan::{TransferPlan, validate_transfer_plan_at};

pub(crate) struct PublicationRecorder<'a> {
    pub state: &'a ApiState,
    pub subscribe: &'a Subscribe,
    pub lease: &'a crate::library_lifecycle::LibraryWorkLease,
    pub fingerprint: store::ReadFingerprint,
}

impl PublicationRecorder<'_> {
    fn prepare_with_plan(
        &self, candidate: &domain::FileCoverage, source: &std::path::Path, mode: library::TransferMode,
        plan: Option<&TransferPlan>,
        ignored_files: &std::collections::HashSet<String>,
    ) -> Result<domain::FileCoverage, subscribe::SubscribeError> {
        let result = (|| -> Result<domain::FileCoverage, String> {
            let store = self.state.store.lock();
            // This lock spans validation and reservation, never filesystem work.
            if let Some(plan) = plan {
                validate_transfer_plan_at(
                    &store,
                    self.subscribe,
                    self.lease,
                    Some(&self.fingerprint),
                    ignored_files,
                    plan,
                )?;
            } else {
                self.lease.control().check().map_err(|error| error.to_string())?;
                if !store.library_accepts_work(self.lease.library_id()).map_err(|e| e.to_string())?
                    || store
                        .read_fingerprint(self.lease.library_id(), self.subscribe.media_id)
                        .map_err(|e| e.to_string())?
                        != self.fingerprint
                    || store.get_subscribe(self.subscribe.id).map_err(|e| e.to_string())?.as_ref() != Some(self.subscribe)
                {
                    return Err("Transfer source or Library configuration changed".into());
                }
            }
            if candidate.file.media_id != self.subscribe.media_id
                || candidate.file.library_id.to_string() != self.lease.library_id()
                || candidate.file.source_path.as_deref() != Some(source)
                || candidate.file.transfer_mode.as_deref() != Some(mode.as_str())
            {
                return Err("Transfer candidate Media, Library, source or mode conflicts with its lease".into());
            }
            // The reservation owns the pending row identity: an interrupted round
            // that left the same destination behind is recovered here rather than
            // re-created, and a conflicting owner/source/mode is refused.
            let scope = store::library_write::WriteScope {
                library_id: candidate.file.library_id,
                config_revision: plan.map_or(0, |plan| plan.config_revision),
            };
            store
                .reserve_transfer(&scope, candidate, source, mode, crate::job_loop::unix_now() * 1000)
                .map_err(|e| e.to_string())
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
        let scope = store::library_write::WriteScope {
            library_id: candidate.file.library_id,
            config_revision: plan.map_or(0, |plan| plan.config_revision),
        };
        self.state
            .store
            .lock()
            .finish_transfer(
                &scope,
                candidate,
                &candidate.file.version,
                crate::job_loop::unix_now() * 1000,
            )
            .map_err(|error| {
                tracing::error!(path = %candidate.file.path.display(), %error, "Transfer publication completion failed");
                subscribe::SubscribeError::Downloader(downloader::DownloaderError::Message(error.to_string()))
            })
    }
}

impl subscribe::TransferRecorder for PublicationRecorder<'_> {
    fn prepare(&self, candidate: &domain::FileCoverage, source: &std::path::Path, mode: library::TransferMode)
        -> Result<domain::FileCoverage, subscribe::SubscribeError>
    {
        self.prepare_with_plan(
            candidate,
            source,
            mode,
            None,
            &std::collections::HashSet::new(),
        )
    }

    fn published(&self, candidate: &domain::FileCoverage) -> Result<(), subscribe::SubscribeError> {
        self.published_with_plan(candidate, None)
    }
}

pub(crate) struct PlannedPublicationRecorder<'a> {
    pub(crate) recorder: PublicationRecorder<'a>,
    pub(crate) plan: &'a TransferPlan,
    /// File rows this round has already reserved/published. Their appearance in
    /// the scoped fingerprint is this round's own effect, not an external change.
    reserved: std::sync::Mutex<std::collections::HashSet<String>>,
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
        }
    }
}

impl subscribe::TransferRecorder for PlannedPublicationRecorder<'_> {
    fn prepare(&self, candidate: &domain::FileCoverage, source: &std::path::Path, mode: library::TransferMode)
        -> Result<domain::FileCoverage, subscribe::SubscribeError>
    {
        let mut reserved = self
            .reserved
            .lock()
            .map_err(|_| {
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
        }
        result
    }

    fn published(&self, candidate: &domain::FileCoverage) -> Result<(), subscribe::SubscribeError> {
        self.recorder.published_with_plan(candidate, Some(self.plan))
    }
}
