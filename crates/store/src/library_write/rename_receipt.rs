//! Lock-free disk execution of a durable rename and the receipt it hands back (C10).
//!
//! [`execute_rename_disk`] never touches the Store. It moves the video (link +
//! unlink on one filesystem, staged copy otherwise) and returns a
//! [`RenameDiskReceipt`] with the destination's native identity. The caller then
//! records it with `Store::record_rename_disk` in a short transaction. Every
//! crash point stays provable by identity:
//!
//! | crash after            | proof that `to` is ours                    |
//! |------------------------|--------------------------------------------|
//! | link (same FS)         | `to` has the source inode                  |
//! | copy published         | `to` has the staging inode (staging kept)  |
//! | source removed         | staging inode, still kept                  |
//! | receipt recorded       | persisted receipt identity                 |
//!
//! Staging is removed only after the receipt is durable. Size and mtime are
//! never evidence, so a foreign file of identical size/time is refused and kept.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::rename_disk::{RenameSourceEvidence, refused, stat, stat_source};
use super::rename_durable::FileRenameRequest;
use crate::StoreError;

/// What the disk phase actually produced: the destination's native identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameDiskReceipt {
    pub rename_id: String,
    pub destination: RenameSourceEvidence,
}

/// Run the disk phase for a planned rename. Holds no Store lock; idempotent.
pub fn execute_rename_disk(request: &FileRenameRequest) -> Result<RenameDiskReceipt, StoreError> {
    execute(request, false)
}

/// Test seam: take the cross-filesystem (staged copy) path on one filesystem.
#[doc(hidden)]
pub fn execute_rename_disk_copying(
    request: &FileRenameRequest,
) -> Result<RenameDiskReceipt, StoreError> {
    execute(request, true)
}

fn execute(request: &FileRenameRequest, force_copy: bool) -> Result<RenameDiskReceipt, StoreError> {
    let evidence = &request.source_evidence;
    let (from, to) = (&request.from_path, &request.to_path);
    let staging = staging_path(to, &request.rename_id);
    match (stat_source(from)?, stat(to)?) {
        (None, Some(_)) if owns_destination(request, &staging)? => receipt(request),
        (None, Some(_)) => Err(refused(request, "destination is not this intent's file")),
        (None, None) => Err(StoreError::Missing(format!(
            "rename {}: neither {} nor {} exists on disk",
            request.rename_id,
            from.display(),
            to.display()
        ))),
        (Some(src), _) if !evidence.is_source_meta(&src) => Err(refused(
            request,
            "source no longer matches the planned file",
        )),
        // Crash after link/copy, before the source was removed.
        (Some(_), Some(_)) if owns_destination(request, &staging)? => {
            std::fs::remove_file(from)?;
            sync_parent(from)?;
            receipt(request)
        }
        (Some(_), Some(_)) => Err(refused(request, "destination is occupied")),
        (Some(_), None) => {
            move_bytes(request, &staging, force_copy)?;
            receipt(request)
        }
    }
}

/// `to` carries the source inode, the kept staging inode, or the recorded receipt.
fn owns_destination(request: &FileRenameRequest, staging: &Path) -> Result<bool, StoreError> {
    let Some(dest) = stat(&request.to_path)? else {
        return Ok(false);
    };
    if request.source_evidence.same_inode(&dest) {
        return Ok(true);
    }
    if request
        .disk_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.same_inode(&dest))
    {
        return Ok(true);
    }
    Ok(stat(staging)?.is_some_and(|staged| {
        use std::os::unix::fs::MetadataExt;
        staged.is_file() && staged.dev() == dest.dev() && staged.ino() == dest.ino()
    }))
}

fn receipt(request: &FileRenameRequest) -> Result<RenameDiskReceipt, StoreError> {
    let dest = stat(&request.to_path)?.ok_or_else(|| {
        StoreError::Missing(format!("rename {} destination vanished", request.rename_id))
    })?;
    Ok(RenameDiskReceipt {
        rename_id: request.rename_id.clone(),
        destination: RenameSourceEvidence::observed(request.expected.clone(), &dest),
    })
}

fn move_bytes(
    request: &FileRenameRequest,
    staging: &Path,
    force_copy: bool,
) -> Result<(), StoreError> {
    let (from, to) = (&request.from_path, &request.to_path);
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !force_copy {
        match std::fs::hard_link(from, to) {
            Ok(()) => {
                sync_parent(to)?;
                std::fs::remove_file(from)?;
                return sync_parent(from);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(refused(request, "destination appeared during the move"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {}
            Err(error) => return Err(error.into()),
        }
    }
    copy_through_staging(request, staging)
}

fn copy_through_staging(request: &FileRenameRequest, staging: &Path) -> Result<(), StoreError> {
    let (from, to) = (&request.from_path, &request.to_path);
    // `to` is absent here, so a leftover staging file proves nothing: start over.
    remove_if_present(staging)?;
    std::fs::copy(from, staging)?;
    std::fs::File::open(staging)?.sync_all()?;
    match stat_source(from)? {
        Some(src) if request.source_evidence.is_source_meta(&src) => {}
        _ => {
            remove_if_present(staging)?;
            return Err(refused(request, "source changed during copy"));
        }
    }
    match std::fs::hard_link(staging, to) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            remove_if_present(staging)?;
            return Err(refused(request, "destination appeared during the copy"));
        }
        Err(error) => return Err(error.into()),
    }
    sync_parent(to)?;
    // The staging link stays: it is the recovery proof until the receipt is recorded.
    std::fs::remove_file(from)?;
    sync_parent(from)
}

/// Drop the staging link once the receipt is durable. Missing is fine (replay).
pub(crate) fn remove_staging(request: &FileRenameRequest) -> Result<(), StoreError> {
    remove_if_present(&staging_path(&request.to_path, &request.rename_id))
}

fn remove_if_present(path: &Path) -> Result<(), StoreError> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

fn sync_parent(path: &Path) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn staging_path(to: &Path, rename_id: &str) -> PathBuf {
    let name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    to.with_file_name(format!(".{name}.rename-{rename_id}.part"))
}
