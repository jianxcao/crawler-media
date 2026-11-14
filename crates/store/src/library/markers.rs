mod analysis;
mod merge;
mod refresh_scope;
mod source_guard;
mod target;

pub use analysis::MarkerAnalysisScope;
pub use source_guard::{SourceObservation, SourceSnapshot, observe_sources, observe_sources_with};

use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFileMarker {
    pub file_id: domain::LedgerId,
    pub version: domain::FileVersion,
    pub unit: domain::UnitKey,
    pub intro_start_ms: Option<i64>,
    pub intro_end_ms: Option<i64>,
    pub outro_start_ms: Option<i64>,
    pub outro_end_ms: Option<i64>,
    pub source: String,
    pub locked: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct MarkerResultReplacement {
    pub media_id: domain::MediaId,
    pub season: u32,
    pub markers: Vec<StoredFileMarker>,
    pub targets: Vec<domain::FileCoverage>,
    pub chapter_updates: Vec<(String, Vec<library::ChapterMarker>)>,
}

impl Store {
    pub fn delete_file_marker(
        &self,
        file_id: domain::LedgerId,
        unit: domain::UnitKey,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.delete_file_marker(file_id, unit))
    }

    /// Replace a season's marker rows in one transaction.
    /// Existing results remain readable until this transaction commits.
    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_marker_results(
        &self,
        media_id: domain::MediaId,
        season: u32,
        markers: &[StoredFileMarker],
        chapter_updates: &[(String, Vec<library::ChapterMarker>)],
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| {
            db.replace_marker_results(media_id, season, markers, chapter_updates)
        })
    }

    /// Atomically replace marker rows across one or more seasons.
    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_marker_results_batch(
        &self,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.replace_marker_results_batch(replacements))
    }

    /// Commit all season results and the marker-refresh terminal state together.
    #[cfg(any(test, feature = "test-support"))]
    pub fn complete_marker_refresh(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.complete_marker_refresh(job_id, replacements))
    }

    pub fn get_file_marker(
        &self,
        file_id: domain::LedgerId,
        unit: domain::UnitKey,
    ) -> Result<Option<StoredFileMarker>, StoreError> {
        self.with_library_read(|db| db.get_file_marker(file_id, unit))
    }

    pub fn put_file_marker_checked(
        &self,
        marker: &StoredFileMarker,
        members: &[SourceObservation],
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.put_file_marker_checked(marker, members))
    }
}

/// Replace only the files explicitly covered by this publication. A different
/// file for the same episode is never part of this deletion or lock decision.
fn write_replacement_markers(
    tx: &rusqlite::Connection,
    replacement: &MarkerResultReplacement,
) -> Result<(), StoreError> {
    for target in &replacement.targets {
        for unit in &target.units {
            if unit.season != replacement.season as i32
                && !(unit.is_whole() && replacement.season == 1)
            {
                continue;
            }
            tx.execute(
                "DELETE FROM file_markers WHERE file_id=?1 AND season=?2 AND episode=?3
                 AND (locked=0 OR file_revision!=?4 OR source_version!=?5)",
                params![
                    target.file.file_id.to_string(),
                    unit.season,
                    unit.episode,
                    target.file.version.revision,
                    target.file.version.source_version
                ],
            )?;
        }
    }
    for marker in &replacement.markers {
        upsert_marker_row(tx, marker)?;
    }
    Ok(())
}

fn upsert_marker_row(
    tx: &rusqlite::Connection,
    marker: &StoredFileMarker,
) -> Result<(), StoreError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    tx.execute(
        "INSERT INTO file_markers (file_id,file_revision,source_version,season,episode,intro_start_ms,intro_end_ms,
            outro_start_ms,outro_end_ms,source,locked,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(file_id,season,episode) DO UPDATE SET
            file_revision=excluded.file_revision,source_version=excluded.source_version,
            intro_start_ms=excluded.intro_start_ms,intro_end_ms=excluded.intro_end_ms,
            outro_start_ms=excluded.outro_start_ms,outro_end_ms=excluded.outro_end_ms,
            source=excluded.source,locked=excluded.locked,updated_at=excluded.updated_at
         WHERE file_markers.locked=0 OR excluded.locked=1
            OR file_markers.file_revision!=excluded.file_revision OR file_markers.source_version!=excluded.source_version",
        params![marker.file_id.to_string(),marker.version.revision,marker.version.source_version,
            marker.unit.season,marker.unit.episode,marker.intro_start_ms,marker.intro_end_ms,
            marker.outro_start_ms,marker.outro_end_ms,marker.source,marker.locked,now],
    )?;
    Ok(())
}

/// Terminal transition for a marker refresh job; fails when the job is no longer active.
fn complete_refresh_job(tx: &rusqlite::Connection, job_id: &str) -> Result<(), StoreError> {
    let finished_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    let changed = tx.execute(
        "UPDATE probe_jobs SET status = 'succeeded', finished_at_ms = ?2
         WHERE id = ?1 AND status IN ('queued', 'running')
           AND completed = total AND succeeded = total AND failed = 0",
        params![job_id, finished_at_ms],
    )?;
    if changed == 0 {
        return Err(StoreError::Missing(format!(
            "active completed marker refresh job {job_id}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "markers/tests.rs"]
mod marker_replacement_tests;

#[path = "markers/connection.rs"]
mod connection;
