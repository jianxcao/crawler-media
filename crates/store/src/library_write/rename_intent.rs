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
        self.rename_intent_plan(file_id, expected, from, to, false)
    }

    pub fn plan_file_reconciliation(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
    ) -> Result<RenameIntentPlan, StoreError> {
        self.rename_intent_plan(file_id, expected, from, to, true)
    }

    fn rename_intent_plan(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
        reconciliation: bool,
    ) -> Result<RenameIntentPlan, StoreError> {
        if !from.is_absolute() || !to.is_absolute() {
            return Err(StoreError::Protected(
                "rename paths must be absolute".into(),
            ));
        }
        let owned = load_owned(&self.library, &file_id.to_string(), &from.to_string_lossy())?;
        if owned.revision != expected.revision || owned.source_version != expected.source_version {
            return Err(StoreError::Protected(format!(
                "file {file_id} version changed"
            )));
        }
        let registered = self
            .library
            .prepare("SELECT kind, path FROM scrape_sidecars WHERE file_id=?1")?
            .query_map(params![file_id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let revisions = self
            .library
            .prepare("SELECT library_id, config_revision FROM libraries")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(RenameIntentPlan {
            file_id,
            expected: expected.clone(),
            from: from.into(),
            to: to.into(),
            reconciliation,
            registered,
            ownership: self.library_ownership()?,
            revisions,
        })
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
        let super::rename_intent::ObservedRenameIntent {
            plan,
            evidence,
            mut sidecars,
            scope,
            resolved_to,
            resolved_from,
        } = observed;
        let (file_id, expected, from, to) = (
            plan.file_id,
            &plan.expected,
            plan.from.as_path(),
            plan.to.as_path(),
        );
        let tx = crate::write_tx(&self.library)?;
        let id = file_id.to_string();
        let from_str = from.to_string_lossy().into_owned();
        crate::pending_file_deletions::ensure_path_not_pending_deletion(&tx, &from_str)?;
        crate::pending_file_deletions::ensure_path_not_pending_deletion(
            &tx,
            &to.to_string_lossy(),
        )?;
        let owned = load_owned(&tx, &id, &from_str)?;
        classify_shared_sidecars(&tx, &id, from, &mut sidecars)?;
        if owned.revision != expected.revision || owned.source_version != expected.source_version {
            return Err(StoreError::Protected(format!("file {id} version changed")));
        }
        ensure_library_writable(&tx, &owned.library_id)?;
        let media_id: MediaId = owned.media_id.parse()?;
        ensure_media_writable(&tx, media_id)?;
        if !scope.matches(&self.library_ownership()?) {
            return Err(StoreError::Conflict(
                "Library roots changed during rename observation".into(),
            ));
        }
        let dest = resolve_destination_owner(&tx, &owned, &resolved_to)?;
        let kind: MediaKind = owned
            .kind
            .parse()
            .map_err(|_| StoreError::Invalid(format!("kind {}", owned.kind)))?;
        if !matches!(scope.owner(&resolved_to, kind), crate::StrictOwner::Owned(owner) if owner.id == dest.library_id)
        {
            return Err(StoreError::Protected(
                "rename destination escaped its Library root".into(),
            ));
        }
        if !matches!(scope.owner(&resolved_from, kind), crate::StrictOwner::Owned(owner) if owner.id == owned.library_id)
        {
            return Err(StoreError::Protected(
                "rename source escaped its recorded Library root".into(),
            ));
        }
        ensure_library_writable(&tx, &dest.library_id)?;
        let to_str = to.to_string_lossy().into_owned();
        refuse_occupied(&tx, &to_str, &id)?;
        let from_config_revision = config_revision(&tx, &owned.library_id)?;
        let to_config_revision = config_revision(&tx, &dest.library_id)?;
        for (library_id, revision) in [
            (&owned.library_id, from_config_revision),
            (&dest.library_id, to_config_revision),
        ] {
            if !plan
                .revisions
                .iter()
                .any(|(id, expected)| id == library_id && *expected == revision)
            {
                return Err(StoreError::Conflict(
                    "Library configuration changed during rename observation".into(),
                ));
            }
        }
        // Destination sidecar targets must also be free of recorded files.
        for side in &sidecars {
            let side_to = side.to.to_string_lossy().into_owned();
            crate::pending_file_deletions::ensure_path_not_pending_deletion(&tx, &side_to)?;
            crate::pending_file_deletions::ensure_path_not_pending_deletion(
                &tx,
                &side.from.to_string_lossy(),
            )?;
            refuse_occupied(&tx, &side_to, &id)?;
        }
        let rename_id = uuid::Uuid::new_v4().to_string();
        let request = FileRenameRequest {
            rename_id,
            file_id,
            expected: expected.clone(),
            from_path: from.to_path_buf(),
            to_path: to.to_path_buf(),
            from_library_id: owned.library_id.parse()?,
            from_root_id: owned.root_id,
            to_library_id: dest.library_id.parse()?,
            to_root_id: dest.root_id,
            sidecars,
            phase: "planned".into(),
            media_id,
            source_evidence: evidence,
            disk_receipt: None,
            from_config_revision,
            to_config_revision,
        };
        insert_request(&tx, &request, now)?;
        tx.commit()?;
        Ok(request)
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
