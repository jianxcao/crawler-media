//! Same-ownership file rename (synchronous ledger update).
//!
//! A rename only changes the recorded path of one file inside the root and
//! Library that already own it. Moving a file to another root or Library is a
//! re-parent and must go through [`super::rename_durable`] (persisted intent,
//! lock-out disk IO, CAS publish). The helpers here stay available for the
//! same-root fast path and for tests that only touch the ledger.

use std::path::{Path, PathBuf};

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
        let tx = crate::write_tx(&self.library)?;
        let id = file_id.to_string();
        let from_str = from.to_string_lossy().into_owned();
        let owned = load_owned(&tx, &id, &from_str)?;
        if owned.revision != expected.revision || owned.source_version != expected.source_version {
            return Err(StoreError::Protected(format!("file {id} version changed")));
        }
        ensure_library_writable(&tx, &owned.library_id)?;
        let media_id: MediaId = owned.media_id.parse()?;
        ensure_media_writable(&tx, media_id)?;
        self.ensure_same_owner(&tx, &owned, to)?;
        let to_str = to.to_string_lossy().into_owned();
        let occupied: i64 = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_files
                           WHERE path_key = norm_path_key(?1) AND file_id <> ?2)",
            params![to_str, id],
            |r| r.get(0),
        )?;
        if occupied != 0 {
            return Err(StoreError::Protected(format!(
                "{to_str} is already recorded"
            )));
        }
        drop(tx);
        Ok(())
    }

    fn ensure_same_owner(
        &self,
        tx: &rusqlite::Connection,
        owned: &Owned,
        to: &Path,
    ) -> Result<(), StoreError> {
        let root: String = tx
            .query_row(
                "SELECT display_path FROM library_roots WHERE root_id = ?1 AND library_id = ?2",
                params![owned.root_id, owned.library_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::Invalid(format!("root {} missing", owned.root_id)))?;
        let outside = || {
            StoreError::Protected(format!(
                "{} is outside the file's own root/Library; re-parenting needs a durable operation",
                to.display()
            ))
        };
        if !to.is_absolute() {
            return Err(outside());
        }
        let resolved_to: PathBuf = resolved_path(to)?;
        if !resolved_to.starts_with(resolved_path(Path::new(&root))?) {
            return Err(outside());
        }
        let kind: MediaKind = owned
            .kind
            .parse()
            .map_err(|_| StoreError::Invalid(format!("kind {}", owned.kind)))?;
        match self.library_for_path_strict(to, kind)? {
            Some(library) if library.id == owned.library_id => Ok(()),
            _ => Err(outside()),
        }
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
