//! Owned observations for planning a durable rename without holding the Store.
use std::path::{Path, PathBuf};

use domain::{FileVersion, LedgerId, MediaId, MediaKind};
use rusqlite::{Connection, params};

use super::publish::{ensure_library_writable, ensure_media_writable};
use super::rename::load_owned;
use super::rename_disk::RenameSourceEvidence;
use super::rename_durable::{
    FileRenameRequest, refuse_occupied, resolve_destination_owner, sidecars_json,
};
use super::rename_durable::{RenameSidecar, merge_registered_sidecars, observe_sidecars};
use super::rename_record::config_revision;
use super::rename_sidecars::{classify_shared_sidecars, observe_directory_artwork};
use crate::{LibraryOwnership, LibraryPathScope, Store, StoreError};

pub struct RenameIntentPlan {
    pub(super) file_id: LedgerId,
    pub(super) expected: FileVersion,
    pub(super) from: PathBuf,
    pub(super) to: PathBuf,
    reconciliation: bool,
    registered: Vec<(String, String)>,
    ownership: LibraryOwnership,
    pub(super) revisions: Vec<(String, i64)>,
}

pub struct ObservedRenameIntent {
    pub(super) plan: RenameIntentPlan,
    pub(super) evidence: RenameSourceEvidence,
    pub(super) sidecars: Vec<RenameSidecar>,
    pub(super) scope: LibraryPathScope,
    pub(super) resolved_to: PathBuf,
    pub(super) resolved_from: PathBuf,
}

impl RenameIntentPlan {
    /// Resolve roots, inspect the source and discover sidecars using owned input.
    pub fn observe(self) -> Result<ObservedRenameIntent, StoreError> {
        if self.reconciliation {
            match std::fs::symlink_metadata(&self.from) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => {
                    return Err(StoreError::Protected(format!(
                        "reconciliation source {} still exists",
                        self.from.display()
                    )));
                }
            }
        }
        let at = if self.reconciliation {
            &self.to
        } else {
            &self.from
        };
        let evidence = RenameSourceEvidence::observe(at, self.expected.clone())?;
        if !self.reconciliation {
            let source = library::observe_source(&self.from).map_err(|error| {
                StoreError::Protected(format!("rename source {}: {error}", self.from.display()))
            })?;
            if source.source_version != self.expected.source_version {
                return Err(StoreError::Protected(format!(
                    "file {} on disk no longer matches its recorded version",
                    self.file_id
                )));
            }
        }
        let mut sidecars = observe_sidecars(&self.from, &self.to);
        observe_directory_artwork(&self.from, &self.to, &mut sidecars);
        merge_registered_sidecars(&self.registered, &self.from, &self.to, &mut sidecars);
        let scope = LibraryPathScope::resolve(self.ownership.clone())?;
        let resolved_to = LibraryPathScope::resolve_path(&self.to)?;
        let resolved_from = LibraryPathScope::resolve_path(&self.from)?;
        Ok(ObservedRenameIntent {
            plan: self,
            evidence,
            sidecars,
            scope,
            resolved_to,
            resolved_from,
        })
    }
}

impl Store {
    pub fn plan_file_rename(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
    ) -> Result<RenameIntentPlan, StoreError> {
        self.with_library_read(|db| db.plan_file_rename(file_id, expected, from, to))
    }

    pub fn plan_file_reconciliation(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
    ) -> Result<RenameIntentPlan, StoreError> {
        self.with_library_read(|db| db.plan_file_reconciliation(file_id, expected, from, to))
    }
}

impl Store {
    /// Observe sidecars beside `from` (lock-free) and persist a rename intent.
    ///
    /// Destination ownership is decided here: same root, same-Library other
    /// root, or another Library. Nested child Libraries and symlink escapes are
    /// rejected. Disk bytes are not touched.
    pub fn begin_file_rename(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
        now: i64,
    ) -> Result<FileRenameRequest, StoreError> {
        self.begin_observed_file_rename(
            self.plan_file_rename(file_id, expected, from, to)?
                .observe()?,
            now,
        )
    }

    /// Ledger-only reconciliation: the bytes already sit at `to` and `from` is gone.
    /// Evidence is observed at `to`, so the disk phase verifies the moved inode
    /// instead of moving anything.
    pub fn begin_file_reconciliation(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
        now: i64,
    ) -> Result<FileRenameRequest, StoreError> {
        self.begin_observed_file_rename(
            self.plan_file_reconciliation(file_id, expected, from, to)?
                .observe()?,
            now,
        )
    }

    /// Persist owned observations; re-check the file, root scope and configuration.
    pub fn begin_observed_file_rename(
        &self,
        observed: super::rename_intent::ObservedRenameIntent,
        now: i64,
    ) -> Result<FileRenameRequest, StoreError> {
        self.with_library_write(|db| db.begin_observed_file_rename(observed, now))
    }
}

fn insert_request(
    tx: &Connection,
    request: &FileRenameRequest,
    now: i64,
) -> Result<(), StoreError> {
    let sidecars_json = sidecars_json(&request.sidecars)?;
    tx.execute(
            "INSERT INTO pending_file_renames
                (rename_id, file_id, from_path, to_path, expected_revision, expected_source_version,
                 from_library_id, from_root_id, to_library_id, to_root_id, sidecars_json, phase,
                 source_size, source_device, source_inode, source_mtime_ns,
                 from_config_revision, to_config_revision, created_at_ms, updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'planned',?12,?13,?14,?15,?16,?17,?18*1000,?18*1000)",
            params![
                request.rename_id,
                request.file_id.to_string(),
                request.from_path.to_string_lossy(),
                request.to_path.to_string_lossy(),
                request.expected.revision,
                request.expected.source_version,
                request.from_library_id.to_string(),
                request.from_root_id,
                request.to_library_id.to_string(),
                request.to_root_id,
                sidecars_json,
                request.source_evidence.size_bytes as i64,
                request.source_evidence.device as i64,
                request.source_evidence.inode as i64,
                request.source_evidence.mtime_ns,
                request.from_config_revision,
                request.to_config_revision,
                now,
            ],
        )?;
    Ok(())
}

#[path = "rename_intent/connection.rs"]
mod connection;
