use rusqlite::{OptionalExtension, params};

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

    pub fn delete_media_markers_for_media(
        &self,
        media_id: domain::MediaId,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM media_markers WHERE media_id = ?1",
            params![media_id.to_string()],
        )?;
        Ok(())
    }

    /// Replace a season's marker rows and chapter caches in one transaction.
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

    /// Atomically replace marker rows and chapter caches across one or more seasons.
    pub fn replace_marker_results_batch(
        &self,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None)
    }

    /// Commit all season results and the marker-refresh terminal state together.
    pub fn complete_marker_refresh(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id))
    }

    fn replace_marker_results_batch_inner(
        &self,
        replacements: &[MarkerResultReplacement],
        completed_job_id: Option<&str>,
    ) -> Result<(), StoreError> {
        let serialized = replacements
            .iter()
            .flat_map(|replacement| {
                replacement
                    .chapter_updates
                    .iter()
                    .map(|(ledger_id, chapters)| {
                        Ok((ledger_id.clone(), serde_json::to_string(chapters)?))
                    })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let tx = self.library.unchecked_transaction()?;
        for replacement in replacements {
            tx.execute(
                "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2",
                params![replacement.media_id.to_string(), replacement.season],
            )?;
            for marker in &replacement.markers {
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
                        marker.updated_at
                    ],
                )?;
            }
        }
        for (ledger_id, chapters_json) in serialized {
            tx.execute(
                "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, chapters_json)
                 VALUES (?1, '[]', '[]', ?2)
                 ON CONFLICT(ledger_id) DO UPDATE SET chapters_json = excluded.chapters_json",
                params![ledger_id, chapters_json],
            )?;
        }
        if let Some(job_id) = completed_job_id {
            let finished_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or_default();
            let changed = tx.execute(
                "UPDATE probe_jobs SET status = 'succeeded', finished_at_ms = ?2
                 WHERE id = ?1 AND status IN ('queued', 'running')
                   AND completed = total AND failed = 0",
                params![job_id, finished_at_ms],
            )?;
            if changed == 0 {
                return Err(StoreError::Missing(format!(
                    "active completed marker refresh job {job_id}"
                )));
            }
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

#[cfg(test)]
mod marker_replacement_tests {
    use super::{MarkerResultReplacement, Store, StoredMediaMarker};
    use crate::probe_tasks::ProbeJobUnitSpec;

    fn marker(
        media_id: domain::MediaId,
        season: u32,
        source: &str,
        start_ms: i64,
    ) -> StoredMediaMarker {
        StoredMediaMarker {
            media_id,
            season,
            episode: 1,
            intro_start_ms: Some(start_ms),
            intro_end_ms: Some(start_ms + 60_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: source.into(),
            locked: false,
            updated_at: 1,
        }
    }

    #[test]
    fn multi_season_refresh_keeps_old_results_on_failure_and_completes_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let media_id = domain::MediaId::new();
        let job_id = "multi-season-refresh";
        let old_chapters = vec![library::ChapterMarker {
            start_ms: 0,
            end_ms: 300_000,
            title: Some("原章节".into()),
            marker_type: None,
            synthetic: false,
        }];
        for season in [1, 2] {
            store
                .put_media_marker(&marker(media_id, season, "old", season as i64 * 100_000))
                .unwrap();
            store
                .put_cached_chapters(&format!("ledger-s{season}"), &old_chapters)
                .unwrap();
        }
        let specs = [
            ProbeJobUnitSpec {
                ledger_id: "ledger-s1",
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
            },
            ProbeJobUnitSpec {
                ledger_id: "ledger-s2",
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
            },
        ];
        assert!(
            store
                .create_probe_job(
                    job_id,
                    "marker_refresh",
                    &media_id.to_string(),
                    None,
                    "marker-refresh-test",
                    &specs,
                )
                .unwrap()
        );
        for ledger_id in ["ledger-s1", "ledger-s2"] {
            assert!(store.start_probe_unit(job_id, ledger_id).unwrap());
            store
                .finish_probe_unit(job_id, ledger_id, true, None)
                .unwrap();
        }
        let s1_new = marker(media_id, 1, "new", 111_000);
        let s2_duplicate = marker(media_id, 2, "new", 222_000);
        let replacements = [
            MarkerResultReplacement {
                media_id,
                season: 1,
                markers: vec![s1_new],
                chapter_updates: vec![(
                    "ledger-s1".into(),
                    vec![library::ChapterMarker {
                        start_ms: 111_000,
                        end_ms: 171_000,
                        title: Some("新片头".into()),
                        marker_type: Some(library::MarkerType::IntroStart),
                        synthetic: false,
                    }],
                )],
            },
            MarkerResultReplacement {
                media_id,
                season: 2,
                markers: vec![s2_duplicate.clone(), s2_duplicate],
                chapter_updates: vec![("ledger-s2".into(), Vec::new())],
            },
        ];

        assert!(
            store
                .complete_marker_refresh(job_id, &replacements)
                .is_err()
        );
        assert_eq!(
            store.get_probe_job(job_id).unwrap().unwrap().status,
            "running"
        );
        for season in [1, 2] {
            assert_eq!(
                store
                    .get_media_marker(media_id, Some(season), Some(1))
                    .unwrap()
                    .unwrap()
                    .source,
                "old"
            );
            assert_eq!(
                store
                    .get_cached_chapters(&format!("ledger-s{season}"))
                    .unwrap()
                    .unwrap(),
                old_chapters
            );
        }

        let valid_replacements = [
            MarkerResultReplacement {
                media_id,
                season: 1,
                markers: vec![marker(media_id, 1, "new", 111_000)],
                chapter_updates: vec![("ledger-s1".into(), old_chapters.clone())],
            },
            MarkerResultReplacement {
                media_id,
                season: 2,
                markers: vec![marker(media_id, 2, "new", 222_000)],
                chapter_updates: vec![("ledger-s2".into(), old_chapters.clone())],
            },
        ];
        store
            .complete_marker_refresh(job_id, &valid_replacements)
            .unwrap();
        assert_eq!(
            store.get_probe_job(job_id).unwrap().unwrap().status,
            "succeeded"
        );
        for (season, expected_source) in [(1, "new"), (2, "new")] {
            assert_eq!(
                store
                    .get_media_marker(media_id, Some(season), Some(1))
                    .unwrap()
                    .unwrap()
                    .source,
                expected_source
            );
        }
    }
}
