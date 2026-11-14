//! Source-checked automatic marker merge and chapter publication share one transaction.
use super::{SourceObservation, Store, StoreError, StoredFileMarker, source_guard};
use domain::FileCoverage;

type ChapterUpdates = Vec<(String, Vec<library::ChapterMarker>)>;

impl Store {
    /// Merge automatic results while every comparison input still has its
    /// observed version. Locked ranges and missing detected fields are preserved.
    /// This method performs DB work only, including chapter timeline composition.
    pub fn merge_marker_results_checked(
        &self,
        targets: &[FileCoverage],
        members: &[SourceObservation],
        markers: &[StoredFileMarker],
        overwrite: bool,
    ) -> Result<ChapterUpdates, StoreError> {
        self.with_library_write(|db| {
            db.merge_marker_results_checked(targets, members, markers, overwrite)
        })
    }
}

fn matches_target(row: &FileCoverage, marker: &StoredFileMarker) -> bool {
    row.file.file_id == marker.file_id
        && row.file.version == marker.version
        && row.units.contains(&marker.unit)
}

#[path = "merge/connection.rs"]
mod connection;
