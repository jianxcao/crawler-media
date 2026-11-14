//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn merge_marker_analysis_checked(
        &self,
        scope: &super::super::MarkerAnalysisScope<'_>,
        targets: &[FileCoverage],
        members: &[SourceObservation],
        markers: &[StoredFileMarker],
        overwrite: bool,
    ) -> Result<ChapterUpdates, StoreError> {
        let tx = crate::write_tx(self.library)?;
        self.validate_marker_analysis_scope(
            scope,
            targets.iter().map(|row| row.file.file_id),
            members,
        )?;
        let updates = self.merge_marker_results_checked(targets, members, markers, overwrite)?;
        tx.commit()?;
        Ok(updates)
    }

    /// Publish file-local chapter detection with the exact source and coverage
    /// checks in the same transaction as its marker and physical chapter cache.
    pub fn publish_file_chapters_checked(
        &self,
        target: &FileCoverage,
        unit: domain::UnitKey,
        members: &[SourceObservation],
        markers: &[StoredFileMarker],
        chapters: Option<&[library::ChapterMarker]>,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        for member in members {
            source_guard::check_observed(&tx, &member.file_id.to_string(), members)?;
        }
        self.validate_marker_target(target, Some(members))?;
        if !target.units.contains(&unit) {
            return Err(StoreError::Invalid("marker_source_changed".into()));
        }
        for marker in markers {
            if !matches_target(target, marker) || marker.unit != unit {
                return Err(StoreError::Invalid("marker_source_changed".into()));
            }
            self.validate_marker_identity(marker, Some(members))?;
            super::super::upsert_marker_row(&tx, marker)?;
        }
        if let Some(chapters) = chapters {
            // The cache writer registers obligations after these marker writes.
            self.put_cached_chapters(&target.file.file_id.to_string(), chapters)?;
        } else {
            self.ensure_chapter_frames_for_updates(&[(
                target.file.file_id.to_string(),
                Vec::new(),
            )])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn merge_marker_results_checked(
        &self,
        targets: &[FileCoverage],
        members: &[SourceObservation],
        markers: &[StoredFileMarker],
        overwrite: bool,
    ) -> Result<ChapterUpdates, StoreError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let tx = crate::write_tx(self.library)?;
        for member in members {
            source_guard::check_observed(&tx, &member.file_id.to_string(), members)?;
        }
        for row in targets {
            self.validate_marker_target(row, Some(members))?;
        }
        let mut updates = Vec::new();
        for marker in markers {
            if !targets.iter().any(|row| matches_target(row, marker)) {
                return Err(StoreError::Invalid(
                    "marker_source_changed: target not covered".into(),
                ));
            }
            self.validate_marker_identity(marker, Some(members))?;
            updates.extend(self.merge_marker_targets(targets, marker.clone(), overwrite)?);
        }
        let affected = targets
            .iter()
            .map(|row| (row.file.file_id.to_string(), Vec::new()))
            .collect::<Vec<_>>();
        self.ensure_chapter_frames_for_updates(&affected)?;
        tx.commit()?;
        tracing::debug!(
            markers = markers.len(),
            files = updates.len(),
            "automatic markers and chapters committed"
        );
        Ok(updates)
    }

    pub(super) fn merge_marker_targets(
        &self,
        targets: &[FileCoverage],
        marker: StoredFileMarker,
        overwrite: bool,
    ) -> Result<ChapterUpdates, StoreError> {
        let marker = self.merged_marker(marker, overwrite)?;
        // These writers use this connection and participate in the active transaction.
        super::super::upsert_marker_row(self.library, &marker)?;
        let mut updates = Vec::with_capacity(targets.len());
        for row in targets.iter().filter(|row| matches_target(row, &marker)) {
            // File chapter caches contain physical chapters only for multiunit files.
            // Unit marker overlays are composed at read time with explicit identity.
            if row.units.len() != 1 {
                continue;
            }
            let id = row.file.file_id.to_string();
            let existing = self.get_cached_chapters(&id)?.unwrap_or_default();
            let intro = match (marker.intro_start_ms, marker.intro_end_ms) {
                (Some(s), Some(e)) if e > s => Some((s, e)),
                _ => None,
            };
            let outro = match (marker.outro_start_ms, marker.outro_end_ms) {
                (Some(s), Some(e)) if e > s => Some((s, e)),
                (Some(s), None) => Some((s, s + 60_000)),
                _ => None,
            };
            let chapters = library::build_complete_timeline_chapters(&existing, intro, outro, None);
            updates.push((id, chapters));
        }
        Ok(updates)
    }

    pub(super) fn merged_marker(
        &self,
        mut marker: StoredFileMarker,
        overwrite: bool,
    ) -> Result<StoredFileMarker, StoreError> {
        if let Some(existing) = self.get_file_marker(marker.file_id, marker.unit)? {
            if !overwrite {
                if existing.locked {
                    marker.intro_start_ms = existing.intro_start_ms;
                    marker.intro_end_ms = existing.intro_end_ms;
                    marker.outro_start_ms = existing.outro_start_ms;
                    marker.outro_end_ms = existing.outro_end_ms;
                    marker.source = existing.source;
                } else {
                    marker.intro_start_ms = marker.intro_start_ms.or(existing.intro_start_ms);
                    marker.intro_end_ms = marker.intro_end_ms.or(existing.intro_end_ms);
                    marker.outro_start_ms = marker.outro_start_ms.or(existing.outro_start_ms);
                    marker.outro_end_ms = marker.outro_end_ms.or(existing.outro_end_ms);
                }
            }
            marker.locked = existing.locked && !overwrite;
        }
        Ok(marker)
    }
}
