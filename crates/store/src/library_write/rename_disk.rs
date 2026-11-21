//! Disk phase of a durable rename, bound to the source evidence captured at
//! `begin` (R01). Runs with no Store lock.
//!
//! * The source is re-verified against its native stat identity before any
//!   byte moves; a replaced or changed source is refused, never moved.
//! * Same filesystem: `link(from,to)` + `unlink(from)` — `link` refuses an
//!   existing destination, so a racing occupant is never clobbered, and a crash
//!   between the two calls leaves `to` with the original inode (provable).
//! * Cross filesystem: copy to a private staging file beside `to`, fsync, link
//!   the staging into place (no-clobber), then remove the source. The staging
//!   link is kept until the Store recorded the receipt, so the destination's
//!   inode is provably this intent's at every crash point.
//! * Recovery never trusts a mere `!from && to.exists()`, nor size + mtime: the
//!   destination must carry the source inode, the staging inode or the
//!   persisted receipt identity (see [`super::rename_receipt`]).
use std::fs::Metadata;
use std::os::unix::fs::{DirEntryExt, MetadataExt};
use std::path::Path;

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

    pub(super) fn observed(version: FileVersion, meta: &Metadata) -> Self {
        Self {
            version,
            size_bytes: meta.len(),
            device: meta.dev(),
            inode: meta.ino(),
            mtime_ns: mtime_ns(meta),
        }
    }

    pub(super) fn is_source_meta(&self, meta: &Metadata) -> bool {
        self.is_source(meta)
    }

    /// Same native file (device + inode, and the recorded length so a reused
    /// inode of a different file does not pass); never size or time alone.
    pub(super) fn same_inode(&self, meta: &Metadata) -> bool {
        meta.is_file()
            && meta.dev() == self.device
            && meta.ino() == self.inode
            && meta.len() == self.size_bytes
    }
}

pub(super) fn mtime_ns(meta: &Metadata) -> i64 {
    meta.mtime().saturating_mul(1_000_000_000) + meta.mtime_nsec()
}

/// Whether `from` and `to` resolve to the exact same directory entry (rather than
/// two distinct hard links pointing to the same inode). Handles parent symlinks,
/// `.` / `..` segments, external hard links (`nlink >= 2`), and case-insensitive
/// or Unicode-normalizing volumes without mutating the filesystem.
pub(super) fn same_directory_entry(from: &Path, to: &Path) -> Result<bool, StoreError> {
    let (Some(from_meta), Some(to_meta)) = (stat(from)?, stat(to)?) else {
        return Ok(false);
    };
    if from_meta.dev() != to_meta.dev() || from_meta.ino() != to_meta.ino() {
        return Ok(false);
    }
    let (Some(from_parent), Some(to_parent)) = (from.parent(), to.parent()) else {
        return Ok(false);
    };
    let (Some(fp_meta), Some(tp_meta)) = (stat_source(from_parent)?, stat_source(to_parent)?)
    else {
        return Ok(false);
    };
    if fp_meta.dev() != tp_meta.dev() || fp_meta.ino() != tp_meta.ino() {
        return Ok(false);
    }
    let (Some(from_name), Some(to_name)) = (from.file_name(), to.file_name()) else {
        return Ok(false);
    };
    if from_name == to_name || from_meta.nlink() < 2 {
        return Ok(true);
    }
    let (mut from_exact, mut to_exact, mut same_ino_count) = (false, false, 0usize);
    for entry in std::fs::read_dir(from_parent)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == from_name {
            from_exact = true;
        }
        if name == to_name {
            to_exact = true;
        }
        if entry.ino() == from_meta.ino() {
            same_ino_count += 1;
        }
    }
    if from_exact && to_exact {
        return Ok(false);
    }
    if same_ino_count <= 1 {
        return Ok(true);
    }
    if std::fs::canonicalize(from)? == std::fs::canonicalize(to)? {
        return Ok(true);
    }
    Ok(from_name.to_string_lossy().to_lowercase() == to_name.to_string_lossy().to_lowercase())
}

pub(super) fn stat(path: &Path) -> Result<Option<Metadata>, StoreError> {
    observe(std::fs::symlink_metadata(path))
}

pub(super) fn stat_source(path: &Path) -> Result<Option<Metadata>, StoreError> {
    observe(std::fs::metadata(path))
}

fn observe(result: std::io::Result<Metadata>) -> Result<Option<Metadata>, StoreError> {
    match result {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn refused(request: &FileRenameRequest, why: &str) -> StoreError {
    StoreError::Protected(format!(
        "rename {} refused: {why} ({} -> {})",
        request.rename_id,
        request.from_path.display(),
        request.to_path.display()
    ))
}
