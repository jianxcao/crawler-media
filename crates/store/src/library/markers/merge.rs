//! Source-checked automatic marker merge and chapter publication share one transaction.
use super::{SourceObservation, Store, StoreError, StoredMediaMarker, source_guard};
use crate::library_write::{ensure_library_writable, ensure_media_writable};
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
        markers: &[StoredMediaMarker],
        overwrite: bool,
    ) -> Result<ChapterUpdates, StoreError> {
        self.with_library_write(|db| {
            db.merge_marker_results_checked(targets, members, markers, overwrite)
        })
    }
}

fn matches_target(row: &FileCoverage, marker: &StoredMediaMarker) -> bool {
    row.file.media_id == marker.media_id
        && row.units.iter().any(|unit| {
            unit.episode_coordinates() == Some((marker.season, marker.episode))
                || (row.file.kind == domain::MediaKind::Movie
                    && row.units.len() == 1
                    && unit.is_whole()
                    && marker.season == 1
                    && marker.episode == 1)
        })
}

#[path = "merge/connection.rs"]
mod connection;
