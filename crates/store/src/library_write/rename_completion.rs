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
            Some(observe_verified_destination(&request)?)
        } else {
            None
        };
        let leftovers = request
            .sidecars
            .iter()
            .filter_map(|side| {
                if !side.action.deletes_source() || side.from == side.to {
                    return None;
                }
                let expected = side.source_identity.as_ref()?;
                let current = std::fs::symlink_metadata(&side.from).ok()?;
                if !expected.matches(&current) {
                    return None;
                }
                Some(ObservedSidecarDeletion {
                    path: side.from.clone(),
                    identity: Some(expected.deletion_identity()),
                })
            })
            .collect();
        Ok(ObservedRenameCompletion {
            request,
            source,
            leftovers,
        })
    }
}

fn observe_verified_destination(
    request: &FileRenameRequest,
) -> Result<library::SourceObservation, StoreError> {
    let receipt = request.disk_receipt.as_ref().ok_or_else(|| {
        StoreError::Protected(format!(
            "rename {}: missing persisted disk receipt",
            request.rename_id
        ))
    })?;
    let path_meta = super::rename_disk::stat(&request.to_path)?.ok_or_else(|| {
        StoreError::Protected(format!(
            "rename {}: destination {} missing",
            request.rename_id,
            request.to_path.display()
        ))
    })?;
    if !receipt.is_source_meta(&path_meta) {
        return Err(StoreError::Protected(format!(
            "rename {}: destination {} no longer matches recorded disk receipt",
            request.rename_id,
            request.to_path.display()
        )));
    }
    let file = std::fs::File::open(&request.to_path).map_err(|error| {
        StoreError::Protected(format!(
            "rename {}: open destination {}: {error}",
            request.rename_id,
            request.to_path.display()
        ))
    })?;
    let fd_meta = file.metadata().map_err(|error| {
        StoreError::Protected(format!(
            "rename {}: stat destination {}: {error}",
            request.rename_id,
            request.to_path.display()
        ))
    })?;
    if !receipt.is_source_meta(&fd_meta) {
        return Err(StoreError::Protected(format!(
            "rename {}: destination {} changed while observing receipt",
            request.rename_id,
            request.to_path.display()
        )));
    }
    library::observe_open_source(&request.to_path, &file, &fd_meta).map_err(|error| {
        StoreError::Protected(format!(
            "rename {}: observe destination: {error}",
            request.rename_id
        ))
    })
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
