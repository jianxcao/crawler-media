//! Owned filesystem observations for the final rename publication.
use std::path::PathBuf;

use super::rename_durable::FileRenameRequest;
use crate::{Store, StoreError};

pub struct RenameCompletionPlan {
    request: FileRenameRequest,
}

pub struct ObservedRenameCompletion {
    pub(super) request: FileRenameRequest,
    pub(super) source: Option<library::SourceObservation>,
    pub(super) leftovers: Vec<ObservedSidecarDeletion>,
}

pub(super) struct ObservedSidecarDeletion {
    pub path: PathBuf,
    pub identity: Option<(u64, Option<u64>)>,
}

impl RenameCompletionPlan {
    /// Inspect destination bytes and leftover sidecars without a Store borrow.
    pub fn observe(self) -> Result<ObservedRenameCompletion, StoreError> {
        let request = self.request;
        let source = if request.phase == "disk_applied" {
            if !request.to_path.is_file() {
                return Err(StoreError::Protected(format!(
                    "rename {}: destination {} missing",
                    request.rename_id,
                    request.to_path.display()
                )));
            }
            Some(library::observe_source(&request.to_path).map_err(|error| {
                StoreError::Protected(format!(
                    "rename {}: observe destination: {error}",
                    request.rename_id
                ))
            })?)
        } else {
            None
        };
        let leftovers = request
            .sidecars
            .iter()
            .filter(|side| {
                side.action.deletes_source() && side.from != side.to && side.from.is_file()
            })
            .map(|side| ObservedSidecarDeletion {
                path: side.from.clone(),
                identity: crate::file_identity(&side.from.to_string_lossy()),
            })
            .collect();
        Ok(ObservedRenameCompletion {
            request,
            source,
            leftovers,
        })
    }
}

impl Store {
    /// Load only durable DB facts; call `observe` after releasing the Store.
    pub fn plan_file_rename_completion(
        &self,
        rename_id: &str,
    ) -> Result<RenameCompletionPlan, StoreError> {
        self.with_library_read(|db| db.plan_file_rename_completion(rename_id))
    }
}

#[path = "rename_completion/connection.rs"]
mod connection;
