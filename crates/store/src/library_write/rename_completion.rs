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
    pub(super) leftovers: Vec<PathBuf>,
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
            .map(|side| side.from.clone())
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
        let request = self.load_file_rename(rename_id)?;
        if !matches!(
            request.phase.as_str(),
            "disk_applied" | "library_published" | "references_applied"
        ) {
            return Err(StoreError::Protected(format!(
                "rename {rename_id} phase {} cannot complete; apply disk first",
                request.phase
            )));
        }
        Ok(RenameCompletionPlan { request })
    }
}
