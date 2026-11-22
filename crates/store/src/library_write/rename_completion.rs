//! Owned filesystem observations for the final rename publication.
use std::path::PathBuf;

use super::rename_durable::{FileRenameRequest, RenameSidecar};
use super::rename_sidecars::{SidecarAction, SidecarSourceIdentity};
use crate::{Store, StoreError};

pub struct RenameCompletionPlan {
    request: FileRenameRequest,
}

pub struct ObservedRenameCompletion {
    pub(super) request: FileRenameRequest,
    pub(super) effective_sidecars: Vec<RenameSidecar>,
    pub(super) reverted_missing_targets: Vec<(PathBuf, PathBuf)>,
    pub(super) source: Option<library::SourceObservation>,
    pub(super) leftovers: Vec<ObservedSidecarDeletion>,
}

pub(super) struct ObservedSidecarDeletion {
    pub path: PathBuf,
    pub source_identity: SidecarSourceIdentity,
    pub target_path: PathBuf,
    pub target_identity: SidecarSourceIdentity,
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
        let mut effective_sidecars = request.sidecars.clone();
        let mut reverted_missing_targets = Vec::new();
        let mut leftovers = Vec::new();
        for side in effective_sidecars.iter_mut() {
            if !side.action.deletes_source()
                || side.from == side.to
                || super::rename_disk::same_directory_entry(&side.from, &side.to).unwrap_or(true)
            {
                continue;
            }
            let Some(expected_source) = side.source_identity.clone() else {
                continue;
            };
            let Ok(current_source) = std::fs::symlink_metadata(&side.from) else {
                continue;
            };
            if !expected_source.matches(&current_source) {
                continue;
            }
            let target_meta = std::fs::symlink_metadata(&side.to);
            let target_missing = matches!(
                &target_meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            );
            let verified_target = side.target_identity.clone().filter(|expected_target| {
                target_meta
                    .as_ref()
                    .is_ok_and(|meta| expected_target.matches(meta))
            });
            if let Some(target_identity) = verified_target {
                leftovers.push(ObservedSidecarDeletion {
                    path: side.from.clone(),
                    source_identity: expected_source,
                    target_path: side.to.clone(),
                    target_identity,
                });
            } else {
                tracing::warn!(
                    rename_id = %request.rename_id,
                    from = %side.from.display(),
                    to = %side.to.display(),
                    target_missing,
                    "sidecar destination missing or changed before completion; keeping original source"
                );
                if target_missing {
                    reverted_missing_targets.push((side.from.clone(), side.to.clone()));
                }
                side.action = SidecarAction::KeepSource;
                side.source_identity = None;
                side.target_identity = None;
            }
        }
        Ok(ObservedRenameCompletion {
            request,
            effective_sidecars,
            reverted_missing_targets,
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
    let observed =
        library::observe_open_source(&request.to_path, &file, &fd_meta).map_err(|error| {
            StoreError::Protected(format!(
                "rename {}: observe destination: {error}",
                request.rename_id
            ))
        })?;
    if let Some(expected_digest) = receipt.source_digest.as_deref()
        && observed.content_digest.as_deref() != Some(expected_digest)
    {
        return Err(StoreError::Protected(format!(
            "rename {}: destination {} content digest changed",
            request.rename_id,
            request.to_path.display()
        )));
    }
    Ok(observed)
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
