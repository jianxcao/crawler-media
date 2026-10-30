//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn delete_media_marker(
        &self,
        media_id: domain::MediaId,
        season: impl Into<Option<u32>>,
        episode: impl Into<Option<u32>>,
    ) -> Result<(), StoreError> {
        let season = season.into().unwrap_or(1);
        let episode = episode.into().unwrap_or(1);
        self.library.execute(
            "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
            params![media_id.to_string(), season, episode],
        )?;
        Ok(())
    }

    pub fn replace_marker_results(
        &self,
        media_id: domain::MediaId,
        season: u32,
        markers: &[StoredMediaMarker],
        chapter_updates: &[(String, Vec<library::ChapterMarker>)],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch(&[MarkerResultReplacement {
            media_id,
            season,
            markers: markers.to_vec(),
            chapter_updates: chapter_updates.to_vec(),
        }])
    }

    pub fn replace_marker_results_batch(
        &self,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None, None)
    }

    pub fn complete_marker_refresh(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id), None)
    }

    pub(super) fn replace_marker_results_batch_inner(
        &self,
        replacements: &[MarkerResultReplacement],
        completed_job_id: Option<&str>,
        observed: Option<&[source_guard::SourceObservation]>,
    ) -> Result<(), StoreError> {
        let check_sources = observed.is_some();
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

        for replacement in replacements {
            refresh_scope::ensure_supported(&tx, replacement)?;
            let covered_episodes = if completed_job_id.is_some() && check_sources {
                Some(refresh_scope::covered_episodes(&tx, replacement)?)
            } else {
                None
            };
            write_replacement_markers(&tx, replacement, covered_episodes.as_ref())?;
        }
        if let Some(job_id) = completed_job_id {
            complete_refresh_job(&tx, job_id)?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn get_media_marker(
        &self,
        media_id: domain::MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Option<StoredMediaMarker>, StoreError> {
        let season = season.unwrap_or(1);
        let episode = episode.unwrap_or(1);
        self.library
            .query_row(
                "SELECT intro_start_ms, intro_end_ms, outro_start_ms, outro_end_ms, source, locked, updated_at
                 FROM media_markers
                 WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
                params![media_id.to_string(), season, episode],
                |row| {
                    Ok(StoredMediaMarker {
                        media_id,
                        season,
                        episode,
                        intro_start_ms: row.get(0)?,
                        intro_end_ms: row.get(1)?,
                        outro_start_ms: row.get(2)?,
                        outro_end_ms: row.get(3)?,
                        source: row.get(4)?,
                        locked: row.get::<_, i64>(5)? != 0,
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn put_media_marker(&self, marker: &StoredMediaMarker) -> Result<(), StoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.library.execute(
            "INSERT INTO media_markers (
                media_id, season, episode, intro_start_ms, intro_end_ms, outro_start_ms, outro_end_ms, source, locked, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(media_id, season, episode) DO UPDATE SET
                intro_start_ms = excluded.intro_start_ms,
                intro_end_ms = excluded.intro_end_ms,
                outro_start_ms = excluded.outro_start_ms,
                outro_end_ms = excluded.outro_end_ms,
                source = excluded.source,
                locked = excluded.locked,
                updated_at = excluded.updated_at
             WHERE media_markers.locked = 0 OR excluded.locked = 1",
            params![
                marker.media_id.to_string(),
                marker.season,
                marker.episode,
                marker.intro_start_ms,
                marker.intro_end_ms,
                marker.outro_start_ms,
                marker.outro_end_ms,
                marker.source,
                if marker.locked { 1 } else { 0 },
                now,
            ],
        )?;
        Ok(())
    }
}
