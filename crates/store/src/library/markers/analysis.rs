//! Publication scope for ordinary season analysis, separate from its references.
use super::{SourceObservation, StoreError, source_guard};
use domain::{FileCoverage, LedgerId};
use std::collections::HashSet;

pub struct MarkerAnalysisScope<'a> {
    pub job_id: Option<&'a str>,
    pub anchor: LedgerId,
    pub comparisons: &'a [FileCoverage],
}

impl crate::LibraryConnection<'_> {
    pub(super) fn validate_marker_analysis_scope(
        &self,
        scope: &MarkerAnalysisScope<'_>,
        targets: impl Iterator<Item = LedgerId>,
        members: &[SourceObservation],
    ) -> Result<(), StoreError> {
        let targets = targets.collect::<Vec<_>>();
        if let Some(job_id) = scope.job_id {
            source_guard::validate_live_job_targets(
                self.library,
                job_id,
                targets.iter().map(ToString::to_string),
            )?;
        } else if targets.iter().any(|id| *id != scope.anchor) {
            return Err(StoreError::Invalid(
                "marker target outside analysis anchor".into(),
            ));
        }
        let compared = scope
            .comparisons
            .iter()
            .map(|row| row.file.file_id)
            .collect::<HashSet<_>>();
        let observed = members
            .iter()
            .map(|member| member.file_id)
            .collect::<HashSet<_>>();
        if compared != observed || targets.iter().any(|id| !compared.contains(id)) {
            return Err(StoreError::Invalid(
                "marker comparison membership changed".into(),
            ));
        }
        for row in scope.comparisons {
            self.validate_marker_target(row, Some(members))?;
        }
        let seasons = scope
            .comparisons
            .iter()
            .flat_map(|row| {
                row.units.iter().map(|unit| {
                    (
                        row.file.media_id,
                        if unit.is_whole() { 1 } else { unit.season },
                    )
                })
            })
            .collect::<HashSet<_>>();
        for (media_id, season) in seasons {
            let expected = scope
                .comparisons
                .iter()
                .filter(|row| {
                    row.file.media_id == media_id
                        && row
                            .units
                            .iter()
                            .any(|unit| unit.season == season || (unit.is_whole() && season == 1))
                })
                .map(|row| row.file.file_id)
                .collect::<HashSet<_>>();
            let actual = self
                .file_coverage_for_media(media_id)?
                .into_iter()
                .filter(|row| {
                    row.units
                        .iter()
                        .any(|unit| unit.season == season || (unit.is_whole() && season == 1))
                })
                .map(|row| row.file.file_id)
                .collect::<HashSet<_>>();
            if actual != expected {
                return Err(StoreError::Missing(
                    "file_deleted: comparison membership changed".into(),
                ));
            }
        }
        Ok(())
    }
}
