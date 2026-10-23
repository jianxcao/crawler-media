//! Disk phase of a durable rename, bound to the source evidence captured at
//! `begin` (R01). Runs with no Store lock.
//!
//! * The source is re-verified against its native stat identity before any
//!   byte moves; a replaced or changed source is refused, never moved.
//! * Same filesystem: `link(from,to)` + `unlink(from)` — `link` refuses an
//!   existing destination, so a racing occupant is never clobbered, and a crash
//!   between the two calls leaves `to` with the original inode (provable).
//! * Cross filesystem: copy to a private temp beside `to`, stamp the source
//!   mtime, link the temp into place (no-clobber), then remove the source. The
//!   copy is recognised on recovery by size + source mtime on another device.
//! * Recovery never trusts a mere `!from && to.exists()`: the destination must
//!   carry the evidence identity (or the copy stamp) to count as this intent's.
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use domain::FileVersion;
use serde::{Deserialize, Serialize};

use super::rename_durable::FileRenameRequest;
use crate::StoreError;

/// Native stat identity of the rename source as observed at `begin`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSourceEvidence {
    pub version: FileVersion,
    pub size_bytes: u64,
    pub device: u64,
    pub inode: u64,
    pub mtime_ns: i64,
}

impl RenameSourceEvidence {
    pub(crate) fn observe(path: &Path, version: FileVersion) -> Result<Self, StoreError> {
        let meta = std::fs::metadata(path).map_err(|error| {
            StoreError::Protected(format!("rename source {}: {error}", path.display()))
        })?;
        if !meta.is_file() {
            return Err(StoreError::Protected(format!(
                "rename source {} is not a regular file",
                path.display()
            )));
        }
        Ok(Self {
            version,
            size_bytes: meta.len(),
            device: meta.dev(),
            inode: meta.ino(),
            mtime_ns: mtime_ns(&meta),
        })
    }

    fn is_source(&self, meta: &Metadata) -> bool {
        meta.is_file()
            && meta.len() == self.size_bytes
            && meta.dev() == self.device
            && meta.ino() == self.inode
            && mtime_ns(meta) == self.mtime_ns
    }

    /// `to` is this intent's moved inode or its cross-device copy.
    fn is_moved(&self, meta: &Metadata) -> bool {
        meta.is_file()
            && meta.len() == self.size_bytes
            && ((meta.dev() == self.device && meta.ino() == self.inode)
                || (meta.dev() != self.device && mtime_ns(meta) == self.mtime_ns))
    }
}

fn mtime_ns(meta: &Metadata) -> i64 {
    meta.mtime().saturating_mul(1_000_000_000) + meta.mtime_nsec()
}

fn stat(path: &Path) -> Result<Option<Metadata>, StoreError> {
    observe(std::fs::symlink_metadata(path))
}

fn stat_source(path: &Path) -> Result<Option<Metadata>, StoreError> {
    observe(std::fs::metadata(path))
}

fn observe(result: std::io::Result<Metadata>) -> Result<Option<Metadata>, StoreError> {
    match result {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn refused(request: &FileRenameRequest, why: &str) -> StoreError {
    StoreError::Protected(format!(
        "rename {} refused: {why} ({} -> {})",
        request.rename_id,
        request.from_path.display(),
        request.to_path.display()
    ))
}

/// Lock-free disk phase: move the video only. Sidecars move in
/// [`crate::Store::apply_file_rename_disk`] after an ownership re-check.
pub fn move_rename_bytes(request: &FileRenameRequest) -> Result<(), StoreError> {
    let evidence = &request.source_evidence;
    let (from, to) = (&request.from_path, &request.to_path);
    match (stat_source(from)?, stat(to)?) {
        (None, Some(dest)) if evidence.is_moved(&dest) => Ok(()),
        (None, Some(_)) => Err(refused(request, "destination is not this intent's file")),
        (None, None) => Err(StoreError::Missing(format!(
            "rename {}: neither {} nor {} exists on disk",
            request.rename_id,
            from.display(),
            to.display()
        ))),
        (Some(src), _) if !evidence.is_source(&src) => Err(refused(
            request,
            "source no longer matches the planned file",
        )),
        // Crash after link/copy, before the source was removed.
        (Some(_), Some(dest)) if evidence.is_moved(&dest) => {
            std::fs::remove_file(from)?;
            Ok(())
        }
        (Some(_), Some(_)) => Err(refused(request, "destination is occupied")),
        (Some(_), None) => move_verified(request),
    }
}

fn move_verified(request: &FileRenameRequest) -> Result<(), StoreError> {
    let (from, to) = (&request.from_path, &request.to_path);
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::hard_link(from, to) {
        Ok(()) => {
            std::fs::remove_file(from)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(refused(request, "destination appeared during the move"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_across_devices(request)
        }
        Err(error) => Err(error.into()),
    }
}

fn copy_across_devices(request: &FileRenameRequest) -> Result<(), StoreError> {
    let (from, to) = (&request.from_path, &request.to_path);
    let evidence = &request.source_evidence;
    let temp = temp_path(to, &request.rename_id);
    let _ = std::fs::remove_file(&temp);
    let result = (|| {
        std::fs::copy(from, &temp)?;
        let file = std::fs::OpenOptions::new().write(true).open(&temp)?;
        let stamp = UNIX_EPOCH + Duration::from_nanos(evidence.mtime_ns.max(0) as u64);
        file.set_modified(stamp)?;
        file.sync_all()?;
        // The source must still be the planned file after the copy.
        match stat_source(from)? {
            Some(src) if evidence.is_source(&src) => {}
            _ => return Err(refused(request, "source changed during copy")),
        }
        match std::fs::hard_link(&temp, to) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(refused(request, "destination appeared during the copy"))
            }
            other => other.map_err(StoreError::from),
        }
    })();
    let _ = std::fs::remove_file(&temp);
    result?;
    std::fs::remove_file(from)?;
    Ok(())
}

fn temp_path(to: &Path, rename_id: &str) -> PathBuf {
    let name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    to.with_file_name(format!(".{name}.rename-{rename_id}.part"))
}
