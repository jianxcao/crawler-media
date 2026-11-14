//! Derive file-wide obligations from current adopted facts on the publication connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn current_chapter_frame_targets(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<Vec<(i64, i64)>>, StoreError> {
        self.file_coverage_by_id(file_id)?
            .map(|row| self.file_chapter_frame_targets(&row))
            .transpose()
    }

    /// The update ids identify affected files. Their producer timelines may have
    /// been composed before locked-marker merging; read the committed facts here.
    /// This is SQL/composition only and borrows the caller's active transaction.
    pub fn ensure_chapter_frames_for_updates(
        &self,
        updates: &[(String, Vec<library::ChapterMarker>)],
    ) -> Result<(), StoreError> {
        let mut visited = std::collections::HashSet::new();
        for (id, _) in updates {
            if !visited.insert(id) {
                continue;
            }
            let file_id = id.parse()?;
            let Some(row) = self.file_coverage_by_id(file_id)? else {
                continue;
            };
            if row.file.presence != domain::FilePresence::Present
                || row.file.publication != domain::FilePublication::Published
            {
                continue;
            }
            let Some(library) = self.get_library(&row.file.library_id.to_string())? else {
                continue;
            };
            if !library.extract_chapter_images {
                continue;
            }
            let targets = self.file_chapter_frame_targets(&row)?;
            self.ensure_chapter_frame_task_inner(
                &ChapterFrameTask {
                    id: uuid::Uuid::new_v4().to_string(),
                    file_id,
                    source_version: row.file.version.source_version.clone(),
                    targets_hash: chapter_frame_generation(
                        file_id,
                        &row.file.version.source_version,
                        &targets,
                        1,
                    ),
                    targets,
                },
                false,
                true,
            )?;
        }
        Ok(())
    }

    /// Every covered unit contributes its current displayed timeline. Cached
    /// entries with synthetic=true are derived overlays and are excluded from
    /// physical input. Current markers are composed separately for every unit,
    /// so an earlier producer timeline cannot reintroduce stale marker ranges.
    fn file_chapter_frame_targets(
        &self,
        row: &domain::FileCoverage,
    ) -> Result<Vec<(i64, i64)>, StoreError> {
        let physical = self
            .get_cached_chapters(&row.file.file_id.to_string())?
            .unwrap_or_default()
            .into_iter()
            .filter(|chapter| !chapter.synthetic)
            .collect::<Vec<_>>();
        let mut targets = Vec::new();
        for unit in &row.units {
            let chapters = match self.get_file_marker(row.file.file_id, *unit)? {
                None => physical.clone(),
                Some(marker) => {
                    let intro = marker
                        .intro_start_ms
                        .zip(marker.intro_end_ms)
                        .filter(|(s, e)| e > s);
                    let outro = marker
                        .outro_start_ms
                        .and_then(|start| {
                            marker
                                .outro_end_ms
                                .or_else(|| start.checked_add(60_000))
                                .map(|end| (start, end))
                        })
                        .filter(|(s, e)| e > s);
                    library::build_complete_timeline_chapters(
                        &physical,
                        intro,
                        outro,
                        row.file.duration_ms,
                    )
                }
            };
            targets.extend(
                chapters
                    .into_iter()
                    .filter(|chapter| chapter.start_ms >= 0 && chapter.end_ms > chapter.start_ms)
                    .map(|chapter| (chapter.start_ms, chapter.end_ms)),
            );
        }
        targets.sort_unstable();
        targets.dedup();
        Ok(targets)
    }
}
