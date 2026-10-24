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
        let (owner, root_id) = self.library_root_owning_key(path, kind)?.ok_or_else(|| {
            StoreError::Protected(format!("no owning Library for {}", path.display()))
        })?;
        ensure_library_writable(&self.library, &owner)?;
        Ok(ScanWriteContext {
            library_id: owner.parse()?,
            root_id,
            kind,
            config_revision: self.library_config_revision(&owner)?,
        })
    }

    /// Restore presence and publish an existing file's NFO observations atomically.
    /// A concurrent rename, rebind, deletion, root change or config update rejects them.
    pub fn refresh_scanned_file(
        &self,
        expected: &LibraryFile,
        context: &ScanWriteContext,
        observations: Option<&[crate::metadata::NfoObservation]>,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        context.validate(&tx, &expected.path, expected.kind)?;
        ensure_media_writable(&tx, expected.media_id)?;
        if expected.library_id != context.library_id || expected.root_id != context.root_id {
            return Err(StoreError::Protected("scan file ownership changed".into()));
        }
        crate::library_write::publish_checked::validate_context(&tx, expected, Some(expected))?;
        let before = tx.total_changes();
        if let Some(observations) = observations {
            let target = crate::metadata::nfo_input::NfoInputTarget::from_file(expected)
                .with_canonical_path(&tx);
            if !crate::metadata::nfo_input::record_nfo_observations(&tx, &target, observations)? {
                return Err(StoreError::Protected("scan NFO observation changed".into()));
            }
        }
        let restored = tx.execute(
            "UPDATE library_files SET presence_state='present', missing_at=NULL
            WHERE file_id=?1 AND (missing_at IS NOT NULL OR presence_state<>'present')",
            params![expected.file_id.to_string()],
        )? > 0;
        if tx.total_changes() != before {
            crate::library_write::reduce_library_media(
                &tx,
                expected.library_id,
                expected.media_id,
            )?;
        }
        tx.commit()?;
        Ok(restored)
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
