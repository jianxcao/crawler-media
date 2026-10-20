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
            let dest_path_str = candidate.file.path.display().to_string();
            let mut reserved = candidate.clone();
            if let Some(existing) = store.ledger_by_path(&dest_path_str).map_err(|e| e.to_string())? {
                if existing.media_id != candidate.file.media_id {
                    return Err("Transfer destination is already owned by another Media".into());
                }
                let mut cursor = None;
                let mut recovered_file = None;
                loop {
                    let page = store
                        .library_file_page(
                            candidate.file.library_id,
                            candidate.file.media_id,
                            cursor.as_deref(),
                            100,
                        )
                        .map_err(|e| e.to_string())?;
                    recovered_file = page
                        .files
                        .into_iter()
                        .find(|file| file.path == candidate.file.path);
                    if recovered_file.is_some() || page.next_cursor.is_none() {
                        break;
                    }
                    cursor = page.next_cursor;
                }
                let file = recovered_file
                    .ok_or_else(|| "Transfer destination file identity is unavailable".to_string())?;
                if file.file_id != existing.id
                    || file.publication != domain::FilePublication::Pending
                    || file.library_id != candidate.file.library_id
                    || file.root_id != candidate.file.root_id
                    || file.source_path.as_deref() != Some(source)
                    || file.transfer_mode.as_deref() != Some(mode.as_str())
                {
                    return Err("Transfer destination reservation identity or source changed".into());
                }
                reserved.file = file;
                reserved.units = store
                    .get_file_units(existing.id)
                    .map_err(|e| e.to_string())?;
            }
            reserved.file.publication = domain::FilePublication::Pending;
            let pub_item = store::library_write::FilePublication {
                file: &reserved.file,
                units: &reserved.units,
                expected_version: Some(&reserved.file.version),
                nfo_observation: None,
                probe_required: false,
            };
            let scope = store::library_write::WriteScope {
                library_id: reserved.file.library_id,
                config_revision: plan.map_or(0, |plan| plan.config_revision),
            };
            store
                .publish_files(&scope, &[pub_item], crate::job_loop::unix_now() * 1000)
                .map_err(|e| e.to_string())?;
            Ok(reserved)
        })();
        result.map_err(|error| {
            tracing::error!(path = %candidate.file.path.display(), %error, "Transfer publication reservation failed");
            subscribe::SubscribeError::Downloader(downloader::DownloaderError::Message(error))
        })
    }

    /// Complete the publication of the reserved file. The reservation and this
    /// call carry the same `file_id`, so the row keeps its root, source path and
    /// transfer mode (see `Store::publish_files` COALESCE semantics).
    fn published_with_plan(
        &self,
        candidate: &domain::FileCoverage,
        plan: Option<&TransferPlan>,
    ) -> Result<(), subscribe::SubscribeError> {
        let pub_item = store::library_write::FilePublication {
            file: &candidate.file,
            units: &candidate.units,
            expected_version: None,
            nfo_observation: None,
            probe_required: true,
        };
        let scope = store::library_write::WriteScope {
            library_id: candidate.file.library_id,
            config_revision: plan.map_or(0, |plan| plan.config_revision),
        };
        self.state
            .store
            .lock()
            .publish_files(&scope, &[pub_item], crate::job_loop::unix_now() * 1000)
            .map_err(|error| {
                tracing::error!(path = %candidate.file.path.display(), %error, "Transfer publication completion failed");
                subscribe::SubscribeError::Downloader(downloader::DownloaderError::Message(error.to_string()))
            })?;
        Ok(())
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
        if result.is_ok() {
            reserved.insert(candidate.file.file_id.to_string());
        }
        result
    }

    fn published(&self, candidate: &domain::FileCoverage) -> Result<(), subscribe::SubscribeError> {
        self.recorder.published_with_plan(candidate, Some(self.plan))
    }
}
