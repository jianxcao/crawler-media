use super::ProbeUnit;
#[cfg(test)]
use crate::Store;
use domain::FileCoverage;

pub(super) fn validate_active_unit(
    store: &store::LibraryConnection<'_>,
    unit: &ProbeUnit,
) -> Result<(), crate::store::StoreError> {
    if let Some(job_id) = unit.job_id.as_deref()
        && !store.is_probe_unit_active(job_id, &unit.row.file.file_id.to_string())?
    {
        return Err(crate::store::StoreError::Missing(
            "file_deleted: probe unit cancelled before publication".into(),
        ));
    }
    Ok(())
}

/// Call only while holding Store, immediately before publishing the computed result.
/// Ledger-only publication check; run [`observe_rows`] (outside the lock) first.
pub(super) fn validate_publication_rows(
    store: &store::LibraryConnection<'_>,
    rows: &[FileCoverage],
) -> Result<(), crate::store::StoreError> {
    for row in rows {
        crate::fingerprint_job::source::ensure_source_recorded_on(store, row)
            .map_err(crate::store::StoreError::Missing)?;
    }
    Ok(())
}

/// Disk observation of publication rows. Must run without the store lock.
pub(super) fn observe_rows(rows: &[FileCoverage]) -> Result<(), crate::store::StoreError> {
    for row in rows {
        crate::fingerprint_job::source::observe_source(row)
            .map_err(crate::store::StoreError::Missing)?;
    }
    Ok(())
}

pub(super) fn source_observations(
    rows: &[FileCoverage],
) -> Result<Vec<crate::store::SourceObservation>, crate::store::StoreError> {
    observe_rows(rows)?;
    Ok(rows
        .iter()
        .map(|row| crate::store::SourceObservation {
            file_id: row.file.file_id,
            expected: row.file.version.clone(),
            presence: domain::FilePresence::Present,
        })
        .collect())
}

#[cfg(test)]
pub(super) fn persist_marker(
    store: &Store,
    rows: &[FileCoverage],
    marker: crate::store::StoredFileMarker,
    overwrite: bool,
) -> Result<Vec<(String, Vec<library::ChapterMarker>)>, crate::store::StoreError> {
    let members = rows
        .iter()
        .map(|row| crate::store::SourceObservation {
            file_id: row.file.file_id,
            expected: row.file.version.clone(),
            presence: domain::FilePresence::Present,
        })
        .collect::<Vec<_>>();
    store
        .library_write(|db| persist_markers_checked(db, rows, &[marker], overwrite, &members, None))
}

#[cfg(test)]
fn persist_markers_checked(
    store: &store::LibraryConnection<'_>,
    rows: &[FileCoverage],
    markers: &[crate::store::StoredFileMarker],
    overwrite: bool,
    members: &[crate::store::SourceObservation],
    scope: Option<&crate::store::MarkerAnalysisScope<'_>>,
) -> Result<Vec<(String, Vec<library::ChapterMarker>)>, crate::store::StoreError> {
    let mut live = Vec::new();
    for row in rows.iter().filter(|row| {
        markers.iter().any(|marker| {
            row.file.file_id == marker.file_id
                && row.file.version == marker.version
                && row.units.contains(&marker.unit)
        })
    }) {
        match crate::fingerprint_job::source::ensure_source_recorded_on(store, row) {
            Ok(()) => live.push(row.clone()),
            Err(reason) if crate::fingerprint_job::source::is_file_deleted(&reason) => {
                tracing::info!(file_id = %row.file.file_id, %reason, "skip unavailable marker publication target");
            }
            Err(reason) => return Err(crate::store::StoreError::Missing(reason)),
        }
    }
    match scope {
        Some(scope) => {
            store.merge_marker_analysis_checked(scope, &live, members, markers, overwrite)
        }
        None => store.merge_marker_results_checked(&live, members, markers, overwrite),
    }
}

#[path = "marker_boundaries.rs"]
mod boundaries;
pub(super) use boundaries::validate_absence_coverage;
