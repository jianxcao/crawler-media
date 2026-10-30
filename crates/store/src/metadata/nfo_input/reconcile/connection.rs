//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn plan_nfo_input_reconcile(
        &self,
        path: &Path,
    ) -> Result<Option<NfoInputReconcilePlan>, StoreError> {
        let Some(existing) = self.existing_file_for_path(path)? else {
            return Ok(None);
        };
        let locale =
            crate::library_query::library_display_locale_on(self.library, existing.library_id)?
                .unwrap_or_default();
        let config_revision = self.library_config_revision(&existing.library_id.to_string())?;
        let target = NfoInputTarget {
            library_id: existing.library_id,
            media_id: existing.media_id,
            root_id: existing.root_id,
            path_key: existing.path_key,
            file_id: existing.file_id,
            version: FileVersion {
                revision: existing.file_revision,
                source_version: existing.source_version,
            },
        };
        Ok(Some(NfoInputReconcilePlan {
            path: path.to_path_buf(),
            kind: existing.kind,
            units: existing.units,
            locale,
            config_revision,
            target,
            key_of: {
                let aliases = crate::path_key::alias_snapshot(self.library);
                std::sync::Arc::new(move |raw: &str| crate::path_key::canonical_key(raw, &aliases))
            },
        }))
    }

    pub fn apply_nfo_input_reconcile(
        &self,
        observed: NfoInputReconcileObserved,
    ) -> Result<bool, StoreError> {
        let NfoInputReconcileObserved { plan, observations } = observed;
        let path = plan.path.as_path();
        let target = &plan.target;
        let tx = crate::write_tx(self.library)?;
        crate::library_operations::integrity_guard::ensure_integrity_writable(
            &tx,
            &[target.library_id],
        )?;
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(
            &tx,
            target.media_id,
        )?;
        let before = tx.total_changes();
        if !record_nfo_observations(&tx, target, &observations)? {
            tracing::warn!(
                path = %path.display(),
                "NFO sidecar 观察在落库前已过期：本次不更新输入，留待下一次扫描"
            );
            return Ok(false);
        }
        // 内容没变时 `record_nfo_observations` 不写库：别为每次重扫白白重算一遍卡片。
        if tx.total_changes() == before {
            return Ok(false);
        }
        crate::library_write::reduce_library_media(&tx, target.library_id, target.media_id)?;
        tx.commit()?;
        tracing::info!(
            path = %path.display(),
            candidates = observations.len(),
            "NFO sidecar 变化已重算卡片"
        );
        Ok(true)
    }

    pub(super) fn existing_file_for_path(
        &self,
        path: &Path,
    ) -> Result<Option<ExistingFile>, StoreError> {
        let key = crate::path_key::canonical_recorded_key(self.library, &path.to_string_lossy());
        let row = self
            .library
            .query_row(
                "SELECT file_id, library_id, media_id, root_id, kind, file_revision, source_version, path_key
                 FROM library_files WHERE path_key = norm_path_key(?1) LIMIT 1",
                params![key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            file_id,
            library_id,
            media_id,
            root_id,
            kind,
            file_revision,
            source_version,
            path_key,
        )) = row
        else {
            return Ok(None);
        };
        let file_id: LedgerId = file_id.parse()?;
        Ok(Some(ExistingFile {
            library_id: library_id.parse()?,
            media_id: media_id.parse()?,
            root_id,
            kind: kind.parse().map_err(StoreError::MediaKind)?,
            file_revision,
            source_version: source_version.unwrap_or_default(),
            units: self.units_of_file(file_id)?,
            file_id,
            path_key,
        }))
    }

    pub(super) fn units_of_file(&self, file_id: LedgerId) -> Result<Vec<UnitKey>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT season, episode FROM library_file_units
             WHERE file_id = ?1 ORDER BY season, episode",
        )?;
        statement
            .query_map(params![file_id.to_string()], |row| {
                Ok(UnitKey::new(row.get::<_, i32>(0)?, row.get::<_, i32>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}
