//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn get_file_version(&self, file_id: LedgerId) -> Result<Option<FileVersion>, StoreError> {
        self.library
            .query_row(
                SELECT_FILE_VERSION,
                params![file_id.to_string()],
                map_version,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn publish_probe_result(
        &self,
        file_id: LedgerId,
        p: &ProbePublication<'_>,
        _now_ms: i64,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let id = file_id.to_string();
        let Some((library_id, media_id)) = file_owner_tx(&tx, &id)? else {
            return Ok(false);
        };
        if current_version_tx(&tx, &id)? != Some(p.expected.clone()) {
            return Ok(false);
        }
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx, media_id)?;
        let before = read_file_summary_facts(&tx, &id)?;
        write_quality_tx(&tx, &id, p)?;
        if let Some(details) = p.details {
            let merged = merge_details_tx(&tx, &id, p.expected, details)?;
            upsert_file_details(&tx, &id, &merged)?;
        }
        // A probe changes only this file's quality columns and cold details; its
        // coverage is untouched. Reduce from this file's before/after facts so one
        // probe never re-reads every other file of the Media (review L2).
        let after = read_file_summary_facts(&tx, &id)?
            .ok_or_else(|| StoreError::Missing(format!("file {id}")))?;
        match before {
            Some(before) if before.published || after.published => {
                let units = read_file_units(&tx, &id)?;
                reduce_library_media_incremental(
                    &tx,
                    library_id,
                    media_id,
                    &ChangedFile {
                        before: Some(&before),
                        before_units: &units,
                        after: &after,
                        after_units: &units,
                    },
                )?;
            }
            Some(_) => {}
            None => reduce_library_media(&tx, library_id, media_id)?,
        }
        tx.commit()?;
        Ok(true)
    }
}
