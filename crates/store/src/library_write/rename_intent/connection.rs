//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
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

    pub(super) fn rename_intent_plan(
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
        let owned = load_owned(self.library, &file_id.to_string(), &from.to_string_lossy())?;
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

    pub fn begin_observed_file_rename(
        &self,
        observed: super::super::rename_intent::ObservedRenameIntent,
        now: i64,
    ) -> Result<FileRenameRequest, StoreError> {
        let super::super::rename_intent::ObservedRenameIntent {
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
        let tx = crate::write_tx(self.library)?;
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
