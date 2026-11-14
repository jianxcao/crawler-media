//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn delete_file_marker(
        &self,
        file_id: domain::LedgerId,
        unit: domain::UnitKey,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        self.library.execute(
            "DELETE FROM file_markers WHERE file_id=?1 AND season=?2 AND episode=?3",
            params![file_id.to_string(), unit.season, unit.episode],
        )?;
        self.ensure_chapter_frames_for_updates(&[(file_id.to_string(), Vec::new())])?;
        tx.commit()?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_marker_results(
        &self,
        media_id: domain::MediaId,
        season: u32,
        markers: &[StoredFileMarker],
        chapter_updates: &[(String, Vec<library::ChapterMarker>)],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch(&[MarkerResultReplacement {
            media_id,
            season,
            targets: chapter_updates
                .iter()
                .map(|(id, _)| {
                    self.file_coverage_by_id(id.parse()?)?
                        .ok_or_else(|| StoreError::Invalid("marker_source_changed".into()))
                })
                .collect::<Result<_, StoreError>>()?,
            markers: markers.to_vec(),
            chapter_updates: chapter_updates.to_vec(),
        }])
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_marker_results_batch(
        &self,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None, None)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn complete_marker_refresh(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id), None)
    }

    pub(crate) fn replace_marker_results_batch_inner(
        &self,
        replacements: &[MarkerResultReplacement],
        completed_job_id: Option<&str>,
        observed: Option<&[source_guard::SourceObservation]>,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        if let Some(members) = observed {
            match completed_job_id {
                Some(job_id) => {
                    // A job-scoped refresh publishes exactly the members the job covered, so the
                    // season-wide membership equality does not apply. Each published file must still
                    // match its out-of-lock observation, and the published set must equal the job's members.
                    source_guard::validate_published_rows(&tx, replacements, members)?;
                    source_guard::validate_job_members(&tx, job_id, replacements, members)?;
                }
                None => source_guard::validate_members(&tx, replacements, members)?,
            }
        }

        let mut identities = std::collections::HashSet::new();
        for replacement in replacements {
            for marker in &replacement.markers {
                if !identities.insert((marker.file_id, marker.unit)) {
                    return Err(StoreError::Invalid("duplicate marker file/unit".into()));
                }
            }
            refresh_scope::ensure_supported(&tx, replacement)?;
            for target in &replacement.targets {
                self.validate_marker_target(target, observed)?;
                if target.file.media_id != replacement.media_id
                    || !target.units.iter().any(|unit| {
                        unit.season == replacement.season as i32
                            || (unit.is_whole() && replacement.season == 1)
                    })
                {
                    return Err(StoreError::Invalid(
                        "marker_source_changed: target outside season".into(),
                    ));
                }
            }
            let target_ids = replacement
                .targets
                .iter()
                .map(|row| row.file.file_id.to_string())
                .collect::<std::collections::HashSet<_>>();
            let chapter_ids = replacement
                .chapter_updates
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<std::collections::HashSet<_>>();
            if target_ids != chapter_ids {
                return Err(StoreError::Invalid(
                    "marker_source_changed: target membership mismatch".into(),
                ));
            }
            for marker in &replacement.markers {
                if !replacement.targets.iter().any(|target| {
                    target.file.file_id == marker.file_id
                        && target.file.version == marker.version
                        && target.units.contains(&marker.unit)
                }) {
                    return Err(StoreError::Invalid(
                        "marker_source_changed: file outside publication".into(),
                    ));
                }
                self.validate_marker_identity(marker, observed)?;
                let row = self
                    .file_coverage_by_id(marker.file_id)?
                    .ok_or_else(|| StoreError::Invalid("marker_source_changed".into()))?;
                if row.file.media_id != replacement.media_id
                    || (!marker.unit.is_whole() && marker.unit.season != replacement.season as i32)
                {
                    return Err(StoreError::Invalid(
                        "marker_source_changed: unit outside season".into(),
                    ));
                }
            }
            write_replacement_markers(&tx, replacement)?;
        }
        let updates = replacements
            .iter()
            .flat_map(|replacement| replacement.targets.iter())
            .map(|row| (row.file.file_id.to_string(), Vec::new()))
            .collect::<Vec<_>>();
        self.ensure_chapter_frames_for_updates(&updates)?;
        if let Some(job_id) = completed_job_id {
            complete_refresh_job(&tx, job_id)?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn get_file_marker(
        &self,
        file_id: domain::LedgerId,
        unit: domain::UnitKey,
    ) -> Result<Option<StoredFileMarker>, StoreError> {
        self.library.query_row(
            "SELECT m.file_revision,m.source_version,m.intro_start_ms,m.intro_end_ms,m.outro_start_ms,m.outro_end_ms,m.source,m.locked,m.updated_at
             FROM file_markers m JOIN library_files f ON f.file_id=m.file_id
                AND f.file_revision=m.file_revision AND f.source_version=m.source_version
             JOIN library_file_units u ON u.file_id=m.file_id AND u.season=m.season AND u.episode=m.episode
             WHERE m.file_id=?1 AND m.season=?2 AND m.episode=?3",
            params![file_id.to_string(),unit.season,unit.episode], |r| Ok(StoredFileMarker {
                file_id,unit,version: domain::FileVersion { revision:r.get(0)?,source_version:r.get(1)? },
                intro_start_ms:r.get(2)?,intro_end_ms:r.get(3)?,outro_start_ms:r.get(4)?,outro_end_ms:r.get(5)?,
                source:r.get(6)?,locked:r.get(7)?,updated_at:r.get(8)?,
            })).optional().map_err(Into::into)
    }

    pub fn put_file_marker_checked(
        &self,
        marker: &StoredFileMarker,
        members: &[SourceObservation],
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        for member in members {
            source_guard::check_observed(&tx, &member.file_id.to_string(), members)?;
        }
        self.validate_marker_identity(marker, Some(members))?;
        upsert_marker_row(&tx, marker)?;
        self.ensure_chapter_frames_for_updates(&[(marker.file_id.to_string(), Vec::new())])?;
        tx.commit()?;
        Ok(())
    }

    /// Called inside the publication transaction; reads only registered facts.
    pub(super) fn validate_marker_identity(
        &self,
        marker: &StoredFileMarker,
        members: Option<&[SourceObservation]>,
    ) -> Result<(), StoreError> {
        let row = self
            .file_coverage_by_id(marker.file_id)?
            .ok_or_else(|| StoreError::Invalid("marker_source_changed".into()))?;
        if row.file.version != marker.version || !row.units.contains(&marker.unit) {
            return Err(StoreError::Invalid("marker_source_changed".into()));
        }
        self.validate_marker_target(&row, members)
    }
}
