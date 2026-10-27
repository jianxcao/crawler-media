mod merge;
mod refresh_scope;
mod source_guard;

pub use source_guard::{SourceObservation, SourceSnapshot, observe_sources, observe_sources_with};

use rusqlite::{OptionalExtension, params, params_from_iter};
use std::collections::BTreeSet;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMediaMarker {
    pub media_id: domain::MediaId,
    pub season: u32,
    pub episode: u32,
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
    pub markers: Vec<StoredMediaMarker>,
    pub chapter_updates: Vec<(String, Vec<library::ChapterMarker>)>,
}

impl Store {
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

    /// Replace a season's marker rows in one transaction.
    /// Existing results remain readable until this transaction commits.
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

    /// Atomically replace marker rows across one or more seasons.
    pub fn replace_marker_results_batch(
        &self,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None, None)
    }

    /// Commit all season results and the marker-refresh terminal state together.
    pub fn complete_marker_refresh(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id), None)
    }

    fn replace_marker_results_batch_inner(
        &self,
        replacements: &[MarkerResultReplacement],
        completed_job_id: Option<&str>,
        observed: Option<&[source_guard::SourceObservation]>,
    ) -> Result<(), StoreError> {
        let check_sources = observed.is_some();
        let tx = crate::write_tx(&self.library)?;
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

/// Replace unlocked marker rows for one season; locked rows are preserved as-is.
/// When `covered_episodes` is `Some`, only episodes in that set are deleted and re-inserted;
/// other episodes keep their marker rows untouched. When `None`, the whole season is replaced.
fn write_replacement_markers(
    tx: &rusqlite::Transaction<'_>,
    replacement: &MarkerResultReplacement,
    covered_episodes: Option<&BTreeSet<i64>>,
) -> Result<(), StoreError> {
    delete_replacement_markers(tx, replacement, covered_episodes)?;
    for marker in &replacement.markers {
        if let Some(episodes) = covered_episodes
            && !episodes.contains(&(marker.episode as i64))
        {
            continue;
        }
        let is_locked: bool = tx
            .query_row(
                "SELECT locked FROM media_markers WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
                params![marker.media_id.to_string(), marker.season, marker.episode],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if is_locked {
            continue;
        }
        insert_marker_row(tx, marker)?;
    }
    Ok(())
}

fn delete_replacement_markers(
    tx: &rusqlite::Transaction<'_>,
    replacement: &MarkerResultReplacement,
    covered_episodes: Option<&BTreeSet<i64>>,
) -> Result<(), StoreError> {
    match covered_episodes {
        None => {
            tx.execute(
                "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2 AND locked = 0",
                params![replacement.media_id.to_string(), replacement.season],
            )?;
        }
        Some(episodes) => {
            if episodes.is_empty() {
                return Ok(());
            }
            let placeholders = vec!["?"; episodes.len()].join(", ");
            let sql = format!(
                "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2 AND locked = 0 AND episode IN ({placeholders})"
            );
            let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(2 + episodes.len());
            let media_id_str = replacement.media_id.to_string();
            params.push(&media_id_str);
            params.push(&replacement.season);
            for ep in episodes {
                params.push(ep);
            }
            let mut stmt = tx.prepare(&sql)?;
            stmt.execute(params_from_iter(params))?;
        }
    }
    Ok(())
}

fn insert_marker_row(
    tx: &rusqlite::Transaction<'_>,
    marker: &StoredMediaMarker,
) -> Result<(), StoreError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    tx.execute(
        "INSERT INTO media_markers (
             media_id, season, episode, intro_start_ms, intro_end_ms,
             outro_start_ms, outro_end_ms, source, locked, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            marker.media_id.to_string(),
            marker.season,
            marker.episode,
            marker.intro_start_ms,
            marker.intro_end_ms,
            marker.outro_start_ms,
            marker.outro_end_ms,
            marker.source,
            marker.locked,
            now,
        ],
    )?;
    Ok(())
}

/// Terminal transition for a marker refresh job; fails when the job is no longer active.
fn complete_refresh_job(tx: &rusqlite::Transaction<'_>, job_id: &str) -> Result<(), StoreError> {
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
