//! Standalone fingerprint/analysis queue summary and targeted cancellation.
use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};

use super::{ProbeJob, ProbeJobUnit, Store, StoreError, counters, map_probe_job, now_ms};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FingerprintCancelScope {
    pub library_id: Option<String>,
    pub media_id: Option<String>,
    pub season: Option<u32>,
    pub file_id: Option<String>,
    pub include_running: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FingerprintCancelSummary {
    pub cancelled_units: usize,
    pub cancelled_file_ids: Vec<String>,
    pub cancelled_job_ids: Vec<String>,
}

impl Store {
    pub fn queued_analysis_unit_count(&self) -> Result<usize, StoreError> {
        self.with_library_read(|db| db.queued_analysis_unit_count())
    }

    pub fn cancel_fingerprint_probe_work(
        &self,
        scope: &FingerprintCancelScope,
        reason: &str,
    ) -> Result<FingerprintCancelSummary, StoreError> {
        self.with_library_write(|db| db.cancel_fingerprint_probe_work(scope, reason))
    }

    pub fn latest_marker_job_for_season(
        &self,
        library_id: domain::LibraryId,
        media_id: &str,
        season: u32,
    ) -> Result<Option<ProbeJob>, StoreError> {
        self.with_library_read(|db| db.latest_marker_job_for_season(library_id, media_id, season))
    }

    pub fn latest_probe_unit_for_ledger(
        &self,
        ledger_id: &str,
    ) -> Result<Option<ProbeJobUnit>, StoreError> {
        self.with_library_read(|db| db.latest_probe_unit_for_ledger(ledger_id))
    }
}

impl crate::LibraryConnection<'_> {
    pub fn queued_analysis_unit_count(&self) -> Result<usize, StoreError> {
        let count: i64 = self.library.query_row(
            "SELECT COUNT(*)
             FROM probe_job_units u
             JOIN probe_jobs j ON j.id = u.job_id
             JOIN library_files f ON f.file_id = u.file_id
             JOIN libraries l ON l.library_id = f.library_id
             WHERE u.stage = 'analysis'
               AND u.status = 'queued'
               AND j.status IN ('queued', 'running')
               AND f.missing_at IS NULL
               AND l.lifecycle_state = 'ready'
               AND NOT EXISTS (SELECT 1 FROM library_removals r WHERE r.library_id = l.library_id)",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn latest_marker_job_for_season(
        &self,
        library_id: domain::LibraryId,
        media_id: &str,
        season: u32,
    ) -> Result<Option<ProbeJob>, StoreError> {
        self.library
            .query_row(
                "SELECT j.id, j.kind, j.media_id, j.season, j.status, j.total, j.completed,
                        j.succeeded, j.failed, j.partial, j.error, j.created_at_ms,
                        j.started_at_ms, j.finished_at_ms
                 FROM probe_jobs j
                 WHERE j.media_id = ?1
                   AND j.season = ?2
                   AND (
                       j.kind IN ('marker_refresh', 'fingerprint_probe')
                       OR (
                           j.kind = 'media_probe'
                           AND j.status IN ('queued', 'running')
                           AND EXISTS (
                               SELECT 1 FROM probe_job_units u
                               WHERE u.job_id = j.id AND u.force_fingerprint != 0
                           )
                       )
                   )
                   AND EXISTS (SELECT 1 FROM probe_job_units u WHERE u.job_id = j.id)
                   AND NOT EXISTS (
                       SELECT 1 FROM probe_job_units u
                       LEFT JOIN library_files f ON f.file_id = u.file_id
                       WHERE u.job_id = j.id AND (f.library_id IS NULL OR f.library_id <> ?3)
                   )
                 ORDER BY j.created_at_ms DESC, j.rowid DESC LIMIT 1",
                params![media_id, season, library_id.to_string()],
                map_probe_job,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_probe_unit_for_ledger(
        &self,
        ledger_id: &str,
    ) -> Result<Option<ProbeJobUnit>, StoreError> {
        self.library
            .query_row(
                "SELECT job_id, file_id, kind, force_fingerprint,
                        reuse_fingerprint_cache, overwrite_markers, status, error,
                        sampling_plan_json, detection_outcome_json, reuse_media_info_cache, stage
                 FROM probe_job_units
                 WHERE file_id = ?1
                 ORDER BY (force_fingerprint != 0 OR detection_outcome_json IS NOT NULL) DESC,
                          rowid DESC
                 LIMIT 1",
                [ledger_id],
                |row| {
                    Ok(ProbeJobUnit {
                        job_id: row.get(0)?,
                        ledger_id: row.get(1)?,
                        kind: row.get(2)?,
                        force_fingerprint: row.get(3)?,
                        reuse_fingerprint_cache: row.get(4)?,
                        overwrite_markers: row.get(5)?,
                        status: row.get(6)?,
                        error: row.get(7)?,
                        sampling_plan_json: row.get(8)?,
                        detection_outcome_json: row.get(9)?,
                        reuse_media_info_cache: row.get::<_, i64>(10).unwrap_or(0) != 0,
                        stage: row
                            .get::<_, String>(11)
                            .ok()
                            .and_then(|value| super::super::ProbeDispatchStage::from_str(&value))
                            .unwrap_or(super::super::ProbeDispatchStage::Metadata),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn cancel_fingerprint_probe_work(
        &self,
        scope: &FingerprintCancelScope,
        reason: &str,
    ) -> Result<FingerprintCancelSummary, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let now = now_ms();
        let targets = select_fingerprint_targets(&tx, scope)?;
        let mut cancelled_file_ids = Vec::with_capacity(targets.len());
        let mut seen_files = BTreeSet::new();
        let mut affected_jobs = BTreeSet::new();
        let mut analysis_targets = Vec::new();

        for (job_id, file_id, stage, _job_kind) in &targets {
            let changed = cancel_unit_row(&tx, job_id, file_id, scope.include_running, reason)?;
            if changed > 0 {
                cancel_stage_state_for_unit(&tx, file_id, job_id, reason, now)?;
                if stage == "analysis" {
                    analysis_targets.push((job_id.clone(), file_id.clone()));
                }
                affected_jobs.insert(job_id.clone());
                if seen_files.insert(file_id.clone()) {
                    cancelled_file_ids.push(file_id.clone());
                }
            }
        }

        for (job_id, file_id) in analysis_targets {
            crate::probe_analysis::transfer_reconciliation_for_cancel_scope(
                &tx,
                &job_id,
                &file_id,
                scope.library_id.as_deref(),
            )?;
        }

        let mut cancelled_job_ids = Vec::new();
        for job_id in affected_jobs {
            reconcile_fingerprint_cancelled_job(
                &tx,
                &job_id,
                scope.include_running,
                reason,
                now,
                &mut seen_files,
                &mut cancelled_file_ids,
            )?;
            cancelled_job_ids.push(job_id);
        }
        tx.commit()?;
        Ok(FingerprintCancelSummary {
            cancelled_units: cancelled_file_ids.len(),
            cancelled_file_ids,
            cancelled_job_ids,
        })
    }
}

fn cancel_unit_row(
    tx: &rusqlite::Connection,
    job_id: &str,
    file_id: &str,
    include_running: bool,
    reason: &str,
) -> Result<usize, StoreError> {
    if include_running {
        tx.execute(
            "UPDATE probe_job_units
             SET status = 'cancelled', error = ?3, analysis_claim_token = NULL
             WHERE job_id = ?1 AND file_id = ?2 AND status IN ('queued', 'running')",
            params![job_id, file_id, reason],
        )
        .map_err(Into::into)
    } else {
        tx.execute(
            "UPDATE probe_job_units
             SET status = 'cancelled', error = ?3
             WHERE job_id = ?1 AND file_id = ?2 AND status = 'queued'",
            params![job_id, file_id, reason],
        )
        .map_err(Into::into)
    }
}

fn cancel_stage_state_for_unit(
    tx: &rusqlite::Connection,
    file_id: &str,
    job_id: &str,
    reason: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE probe_stage_state
         SET status = 'cancelled',
             active_job_id = NULL,
             dispatched_until_ms = NULL,
             next_retry_at_ms = NULL,
             error_kind = 'cancelled',
             error = ?3,
             updated_at_ms = ?4
         WHERE file_id = ?1 AND active_job_id = ?2",
        params![file_id, job_id, reason, now_ms],
    )?;
    Ok(())
}

fn select_fingerprint_targets(
    tx: &rusqlite::Connection,
    scope: &FingerprintCancelScope,
) -> Result<Vec<(String, String, String, String)>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT u.job_id, u.file_id, u.stage, j.kind, f.library_id, f.media_id, j.season
         FROM probe_job_units u
         JOIN probe_jobs j ON j.id = u.job_id
         JOIN library_files f ON f.file_id = u.file_id
         WHERE j.status IN ('queued', 'running')
           AND (
               (u.stage IN ('fingerprint', 'analysis')
                   AND (u.status = 'queued' OR (?1 != 0 AND u.status = 'running')))
               OR (u.stage = 'metadata' AND u.status = 'queued'
                   AND (j.kind = 'marker_refresh'
                        OR (j.kind = 'media_probe' AND u.force_fingerprint <> 0)))
           )",
    )?;
    let rows = stmt.query_map([i64::from(scope.include_running)], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<i64>>(6)?,
        ))
    })?;
    let mut matched = Vec::new();
    for row in rows {
        let (job_id, file_id, stage, job_kind, lib_id, media_id, job_season) = row?;
        if let Some(target_lib) = scope.library_id.as_deref()
            && lib_id != target_lib
        {
            continue;
        }
        if let Some(target_media) = scope.media_id.as_deref()
            && media_id != target_media
        {
            continue;
        }
        if let Some(target_file) = scope.file_id.as_deref()
            && file_id != target_file
        {
            continue;
        }
        if let Some(target_season) = scope.season {
            let matches_season = job_season == Some(i64::from(target_season))
                || file_covers_season(tx, &file_id, target_season)?;
            if !matches_season {
                continue;
            }
        }
        matched.push((job_id, file_id, stage, job_kind));
    }
    Ok(matched)
}

fn file_covers_season(
    tx: &rusqlite::Connection,
    file_id: &str,
    season: u32,
) -> Result<bool, StoreError> {
    tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM library_file_units
             WHERE file_id = ?1 AND (season = ?2 OR (season = -1 AND ?2 = 1))
         )",
        params![file_id, i64::from(season)],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn cancel_remaining_marker_refresh_units(
    tx: &rusqlite::Connection,
    job_id: &str,
    reason: &str,
    now: i64,
    seen_files: &mut BTreeSet<String>,
    cancelled_file_ids: &mut Vec<String>,
) -> Result<(), StoreError> {
    let remaining: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT file_id, stage
             FROM probe_job_units
             WHERE job_id = ?1
               AND (
                   (stage IN ('fingerprint', 'analysis') AND status IN ('queued', 'running'))
                   OR (stage = 'metadata' AND status = 'queued')
               )",
        )?;
        stmt.query_map([job_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut analysis_files = Vec::new();
    for (file_id, stage) in remaining {
        let changed = cancel_unit_row(tx, job_id, &file_id, true, reason)?;
        if changed > 0 {
            cancel_stage_state_for_unit(tx, &file_id, job_id, reason, now)?;
            if stage == "analysis" {
                analysis_files.push(file_id.clone());
            }
            if seen_files.insert(file_id.clone()) {
                cancelled_file_ids.push(file_id);
            }
        }
    }
    for file_id in analysis_files {
        crate::probe_analysis::transfer_reconciliation(tx, job_id, &file_id)?;
    }
    Ok(())
}

fn reconcile_fingerprint_cancelled_job(
    tx: &rusqlite::Connection,
    job_id: &str,
    include_running: bool,
    reason: &str,
    now: i64,
    seen_files: &mut BTreeSet<String>,
    cancelled_file_ids: &mut Vec<String>,
) -> Result<(), StoreError> {
    if crate::probe_analysis::fail_incomplete_marker_refresh_in_tx(tx, job_id, now)? {
        return Ok(());
    }
    let kind: Option<String> = tx
        .query_row(
            "SELECT kind FROM probe_jobs WHERE id = ?1 AND status IN ('queued', 'running')",
            [job_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(kind) = kind else {
        return Ok(());
    };
    if kind == "marker_refresh" && include_running {
        cancel_remaining_marker_refresh_units(
            tx,
            job_id,
            reason,
            now,
            seen_files,
            cancelled_file_ids,
        )?;
    }
    counters::reconcile_counts_in_transaction(tx, job_id)?;
    let row: Option<(usize, usize, usize, usize)> = tx
        .query_row(
            "SELECT total, completed, failed, partial
             FROM probe_jobs WHERE id = ?1 AND status IN ('queued', 'running')",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((total, completed, failed, partial)) = row else {
        return Ok(());
    };
    if completed < total {
        return Ok(());
    }
    let cancelled: usize = tx.query_row(
        "SELECT COUNT(*) FROM probe_job_units WHERE job_id = ?1 AND status = 'cancelled'",
        [job_id],
        |row| row.get(0),
    )?;
    let terminal = if kind == "marker_refresh" && cancelled > 0 {
        Some("cancelled")
    } else {
        super::probe_job_terminal_status(&kind, total, completed, failed, partial, cancelled)
    };
    if let Some(terminal) = terminal {
        tx.execute(
            "UPDATE probe_jobs
             SET status = ?2,
                 finished_at_ms = COALESCE(finished_at_ms, ?3),
                 error = COALESCE(error, ?4)
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![job_id, terminal, now, reason],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ProbeDispatchStage, ProbeJobUnitSpec, ProbeStage, ProbeStageKey, ProbeStageStatus,
    };
    use domain::{Media, MediaId, MediaKind};

    fn seed_cancel_isolation_files(dir: &std::path::Path, store: &Store) -> (MediaId, Vec<String>) {
        let root = dir.join("tv");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "TV",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "Isolation".into(),
                year: Some(2025),
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let files = (1..=3)
            .map(|ep| {
                let p = root.join(format!("S01E{ep:02}.mkv"));
                std::fs::write(&p, b"tv").unwrap();
                crate::test_support::publish_fixture_file(
                    store,
                    &crate::test_support::FixtureFile {
                        media_id,
                        path: p.display().to_string(),
                        season: Some(1),
                        episode: Some(ep),
                        ..Default::default()
                    },
                )
                .unwrap()
                .file_id
                .to_string()
            })
            .collect();
        (media_id, files)
    }

    fn fp_spec(ledger_id: &str, reuse: bool) -> ProbeJobUnitSpec<'_> {
        ProbeJobUnitSpec {
            ledger_id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: reuse,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        }
    }

    #[test]
    fn cancel_fingerprint_preserves_running_metadata_and_clears_sibling_stage_states() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let (media_id, files) = seed_cancel_isolation_files(dir.path(), &store);
        let (f_meta, f_fp1, f_fp2) = (&files[0], &files[1], &files[2]);
        assert!(
            store
                .create_probe_job(
                    "job-meta",
                    "media_probe",
                    &media_id.to_string(),
                    Some(1),
                    &format!("ledger:{f_meta}"),
                    &[fp_spec(f_meta, true)],
                )
                .unwrap()
        );
        assert!(
            store
                .start_probe_unit("job-meta", f_meta, ProbeDispatchStage::Metadata)
                .unwrap()
        );
        assert!(
            store
                .create_probe_job(
                    "job-refresh",
                    "marker_refresh",
                    &media_id.to_string(),
                    Some(1),
                    "marker:s1",
                    &[fp_spec(f_fp1, false), fp_spec(f_fp2, false)],
                )
                .unwrap()
        );
        for f in [f_fp1, f_fp2] {
            assert!(
                store
                    .advance_probe_unit_to_fingerprint("job-refresh", f)
                    .unwrap()
            );
            assert!(
                store
                    .start_probe_unit("job-refresh", f, ProbeDispatchStage::Fingerprint)
                    .unwrap()
            );
            let key = ProbeStageKey {
                ledger_id: f.clone(),
                context_key: "ctx-1".into(),
                stage: ProbeStage::Intro,
            };
            assert!(store.claim_probe_stage(&key, "job-refresh", 100).unwrap());
        }
        let summary = store
            .cancel_fingerprint_probe_work(
                &FingerprintCancelScope {
                    file_id: Some(f_fp1.clone()),
                    include_running: true,
                    ..Default::default()
                },
                "manual_cancel",
            )
            .unwrap();
        assert_eq!(summary.cancelled_units, 2);
        assert!(store.is_probe_unit_active("job-meta", f_meta).unwrap());
        for f in [f_fp1, f_fp2] {
            let st = store
                .get_probe_stage(&ProbeStageKey {
                    ledger_id: f.clone(),
                    context_key: "ctx-1".into(),
                    stage: ProbeStage::Intro,
                })
                .unwrap()
                .unwrap();
            assert_eq!(st.status, ProbeStageStatus::Cancelled);
            assert!(st.active_job_id.is_none());
        }
    }
}
