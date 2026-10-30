//! Same-ownership file rename (synchronous ledger update).
//!
//! A rename only changes the recorded path of one file inside the root and
//! Library that already own it. Moving a file to another root or Library is a
//! re-parent and must go through [`super::rename_durable`] (persisted intent,
//! lock-out disk IO, CAS publish). The helpers here stay available for the
//! same-root fast path and for tests that only touch the ledger.

use std::path::Path;

use domain::{FileVersion, LedgerId, MediaId, MediaKind};
use rusqlite::{OptionalExtension, params};

use super::publish::{ensure_library_writable, ensure_media_writable};
use crate::library_paths::resolved_path;
use crate::{Store, StoreError};

pub(crate) struct Owned {
    pub library_id: String,
    pub root_id: String,
    pub media_id: String,
    pub kind: String,
    pub revision: i64,
    pub source_version: String,
}

impl Store {
    /// Rename `file_id` from `from` to `to` within its own root.
    ///
    /// Revalidates in one transaction: the row still sits at `from` with the
    /// expected version; the Library is writable and the Media is not frozen;
    /// `to` (after `..`/symlink resolution) stays under the file's own root and
    /// is strictly owned by the same Library (not a nested child Library); no
    /// other file row occupies `to`. Then runs the one durable rename policy
    /// (intent, disk move, content-token verification, CAS publish), so sync and
    /// durable renames cannot diverge on source identity.
    pub fn rename_file_within_root(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
    ) -> Result<(), StoreError> {
        self.rename_within_root(file_id, expected, from, to)?;
        let now = crate::unix_now();
        let request = self.begin_file_rename(file_id, expected, from, to, now)?;
        let receipt = super::execute_rename_disk(&request).inspect_err(|error| {
            let _ = self.record_rename_refusal(&request.rename_id, now, error);
        })?;
        self.record_rename_disk(&receipt, now)?;
        self.complete_file_rename(&request.rename_id, now)
    }

    /// Run every [`Self::rename_file_within_root`] check without writing, so a
    /// caller can refuse before it moves any bytes on disk.
    pub fn check_file_rename(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
    ) -> Result<(), StoreError> {
        self.rename_within_root(file_id, expected, from, to)
    }

    fn rename_within_root(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
    ) -> Result<(), StoreError> {
        let libraries = self.list_libraries()?;
        let mut resolved = libraries.clone();
        for library in &mut resolved {
            for root in &mut library.root_paths {
                *root = resolved_path(root)?;
            }
        }
        let resolved_to = resolved_path(to)?;
        self.with_library_read(|db| {
            if db.list_libraries()? != libraries {
                return Err(StoreError::Protected(
                    "Library roots changed during rename preparation".into(),
                ));
            }
            db.rename_within_root(file_id, expected, from, to, &resolved_to, &resolved)
        })
    }
}

pub(crate) fn load_owned(
    tx: &rusqlite::Connection,
    id: &str,
    from: &str,
) -> Result<Owned, StoreError> {
    tx.query_row(
        "SELECT library_id, root_id, media_id, kind, file_revision, source_version
         FROM library_files
         WHERE file_id = ?1 AND path_key = norm_path_key(?2)",
        params![id, from],
        |r| {
            Ok(Owned {
                library_id: r.get(0)?,
                root_id: r.get(1)?,
                media_id: r.get(2)?,
                kind: r.get(3)?,
                revision: r.get(4)?,
                source_version: r.get(5)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| StoreError::Missing(format!("file {id} at {from}")))
}

#[path = "rename/connection.rs"]
mod connection;
