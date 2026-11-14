//! Snapshot ownership before cold scan IO and validate it in the write transaction.
use crate::library_write::{
    WriteScope, ensure_config_revision, ensure_library_writable, ensure_media_writable,
};
use crate::{Store, StoreError};
use domain::{LibraryFile, LibraryId, MediaKind};
use rusqlite::{Connection, params};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct ScanWriteContext {
    pub library_id: LibraryId,
    pub root_id: String,
    pub kind: MediaKind,
    pub config_revision: i64,
}

impl Store {
    /// DB-only ownership snapshot for an observation that will happen without Store.
    pub fn scan_write_context(
        &self,
        path: &Path,
        kind: MediaKind,
    ) -> Result<ScanWriteContext, StoreError> {
        self.with_library_read(|db| db.scan_write_context(path, kind))
    }

    /// Restore presence and publish an existing file's NFO observations atomically.
    /// A concurrent rename, rebind, deletion, root change or config update rejects them.
    pub fn refresh_scanned_file(
        &self,
        expected: &LibraryFile,
        context: &ScanWriteContext,
        observations: Option<&[crate::metadata::NfoObservation]>,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.refresh_scanned_file(expected, context, observations))
    }
}

impl ScanWriteContext {
    pub(crate) fn validate(
        &self,
        tx: &Connection,
        path: &Path,
        kind: MediaKind,
    ) -> Result<(), StoreError> {
        ensure_library_writable(tx, &self.library_id.to_string())?;
        ensure_config_revision(
            tx,
            &WriteScope {
                library_id: self.library_id,
                config_revision: self.config_revision,
            },
        )?;
        let owner = crate::library_key_owner::owning_root_key(tx, path, kind)?;
        if self.kind != kind || owner != Some((self.library_id.to_string(), self.root_id.clone())) {
            return Err(StoreError::Protected(format!(
                "scan ownership changed for {}",
                path.display()
            )));
        }
        Ok(())
    }
}

#[path = "scan_context/connection.rs"]
mod connection;
