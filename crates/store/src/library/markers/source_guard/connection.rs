//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn replace_marker_analysis_checked(
        &self,
        scope: &super::super::MarkerAnalysisScope<'_>,
        replacements: &[MarkerResultReplacement],
        members: &[SourceObservation],
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        self.validate_marker_analysis_scope(
            scope,
            replacements
                .iter()
                .flat_map(|r| r.targets.iter().map(|row| row.file.file_id)),
            members,
        )?;
        self.replace_marker_results_batch_inner(replacements, None, Some(members))?;
        tx.commit()?;
        Ok(())
    }

    pub fn source_snapshots(
        &self,
        file_ids: &[LedgerId],
    ) -> Result<Vec<SourceSnapshot>, StoreError> {
        let mut statement = self.library.prepare_cached(
            "SELECT file_revision, source_version, display_path FROM library_files WHERE file_id = ?1",
        )?;
        let mut snapshots = Vec::with_capacity(file_ids.len());
        for file_id in file_ids {
            let row = statement
                .query_row([file_id.to_string()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .optional()?;
            if let Some((revision, source_version, path)) = row {
                snapshots.push(SourceSnapshot {
                    file_id: *file_id,
                    version: FileVersion {
                        revision,
                        source_version,
                    },
                    path: PathBuf::from(path),
                });
            }
        }
        Ok(snapshots)
    }

    pub fn complete_marker_refresh_checked(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
        members: &[SourceObservation],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id), Some(members))
    }

    pub fn replace_marker_results_batch_checked(
        &self,
        replacements: &[MarkerResultReplacement],
        members: &[SourceObservation],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None, Some(members))
    }
}
