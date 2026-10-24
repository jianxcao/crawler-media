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
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let tx = crate::write_tx(&self.library)?;
        for member in members {
            source_guard::check_observed(&tx, &member.file_id.to_string(), members)?;
        }
        for row in targets {
            let file = &row.file;
            source_guard::check_observed(&tx, &file.file_id.to_string(), members)?;
            crate::library_write::publish_checked::validate_context(&tx, file, Some(file))?;
            ensure_library_writable(&tx, &file.library_id.to_string())?;
            ensure_media_writable(&tx, file.media_id)?;
            let owner = crate::library_key_owner::owning_root_key(&tx, &file.path, file.kind)?;
            if owner != Some((file.library_id.to_string(), file.root_id.clone())) {
                return Err(StoreError::Protected(
                    "marker target ownership changed".into(),
                ));
            }
        }
        let mut updates = Vec::new();
        for marker in markers {
            if targets.iter().any(|row| matches_target(row, marker)) {
                updates.extend(self.merge_marker_targets(targets, marker.clone(), overwrite)?);
            }
        }
        tx.commit()?;
        tracing::debug!(
            markers = markers.len(),
            files = updates.len(),
            "automatic markers and chapters committed"
        );
        Ok(updates)
    }

    fn merge_marker_targets(
        &self,
        targets: &[FileCoverage],
        marker: StoredMediaMarker,
        overwrite: bool,
    ) -> Result<ChapterUpdates, StoreError> {
        let marker = self.merged_marker(marker, overwrite)?;
        // These writers use this connection and participate in the active transaction.
        self.put_media_marker(&marker)?;
        let mut updates = Vec::with_capacity(targets.len());
        for row in targets.iter().filter(|row| matches_target(row, &marker)) {
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
            self.put_cached_chapters(&id, &chapters)?;
            updates.push((id, chapters));
        }
        Ok(updates)
    }

    fn merged_marker(
        &self,
        mut marker: StoredMediaMarker,
        overwrite: bool,
    ) -> Result<StoredMediaMarker, StoreError> {
        if let Some(existing) =
            self.get_media_marker(marker.media_id, Some(marker.season), Some(marker.episode))?
        {
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

fn matches_target(row: &FileCoverage, marker: &StoredMediaMarker) -> bool {
    // Comparison uses the first unit, with whole-work coordinates defaulting to 1.
    let unit = row.units.first();
    let season = unit.map_or(1, |unit| u32::try_from(unit.season).unwrap_or(1));
    let episode = unit.map_or(1, |unit| u32::try_from(unit.episode).unwrap_or(1));
    row.file.media_id == marker.media_id && season == marker.season && episode == marker.episode
}
