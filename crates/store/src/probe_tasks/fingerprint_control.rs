//! Standalone fingerprint/analysis queue summary and targeted cancellation.
use std::collections::BTreeSet;

use rusqlite::params;

use super::{ProbeJob, ProbeJobUnit, Store, StoreError, counters, map_probe_job, now_ms};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FingerprintCancelScope {
    pub library_id: Option<String>,
    pub media_id: Option<String>,
    pub season: Option<u32>,
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
        media_id: &str,
        season: u32,
    ) -> Result<Option<ProbeJob>, StoreError> {
        self.with_library_read(|db| db.latest_marker_job_for_season(media_id, season))
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
               AND u.status IN ('queued', 'running')
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
        media_id: &str,
        season: u32,
    ) -> Result<Option<ProbeJob>, StoreError> {
        use rusqlite::OptionalExtension;
        self.library
            .query_row(
                "SELECT id, kind, media_id, season, status, total, completed, succeeded,
                        failed, partial, error, created_at_ms, started_at_ms, finished_at_ms
                 FROM probe_jobs
                 WHERE kind IN ('marker_refresh', 'fingerprint_probe')
                   AND media_id = ?1
                   AND (season = ?2 OR season IS NULL)
                 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                params![media_id, season],
                map_probe_job,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_probe_unit_for_ledger(
        &self,
        ledger_id: &str,
    ) -> Result<Option<ProbeJobUnit>, StoreError> {
        use rusqlite::OptionalExtension;
        self.library
            .query_row(
                "SELECT job_id, file_id, kind, force_fingerprint,
                        reuse_fingerprint_cache, overwrite_markers, status, error,
                        sampling_plan_json, detection_outcome_json, reuse_media_info_cache, stage
                 FROM probe_job_units
                 WHERE file_id = ?1
                 ORDER BY rowid DESC LIMIT 1",
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
        let targets = select_fingerprint_targets(&tx, scope)?;
        let mut cancelled_file_ids = Vec::with_capacity(targets.len());
        let mut seen_files = BTreeSet::new();
        let mut affected_jobs = BTreeSet::new();
        let mut analysis_targets = Vec::new();

        for (job_id, file_id, stage, _job_kind) in &targets {
            let changed = if scope.include_running {
                tx.execute(
                    "UPDATE probe_job_units
                     SET status = 'cancelled', error = ?3, analysis_claim_token = NULL
                     WHERE job_id = ?1 AND file_id = ?2 AND status IN ('queued', 'running')",
                    params![job_id, file_id, reason],
                )?
            } else {
                tx.execute(
                    "UPDATE probe_job_units
                     SET status = 'cancelled', error = ?3
                     WHERE job_id = ?1 AND file_id = ?2 AND status = 'queued'",
                    params![job_id, file_id, reason],
                )?
            };
            if changed > 0 {
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

        let now = now_ms();
        let mut cancelled_job_ids = Vec::new();
        for job_id in affected_jobs {
            reconcile_fingerprint_cancelled_job(&tx, &job_id, scope.include_running, reason, now)?;
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
           AND (u.stage IN ('fingerprint', 'analysis')
                OR j.kind IN ('fingerprint_probe', 'marker_refresh'))
           AND (u.status = 'queued' OR (?1 != 0 AND u.status = 'running'))",
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

fn reconcile_fingerprint_cancelled_job(
    tx: &rusqlite::Connection,
    job_id: &str,
    include_running: bool,
    reason: &str,
    now: i64,
) -> Result<(), StoreError> {
    use rusqlite::OptionalExtension;
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
        tx.execute(
            "UPDATE probe_job_units
             SET status = 'cancelled', error = ?2, analysis_claim_token = NULL
             WHERE job_id = ?1 AND status IN ('queued', 'running')",
            params![job_id, reason],
        )?;
        counters::reconcile_counts_in_transaction(tx, job_id)?;
        tx.execute(
            "UPDATE probe_jobs
             SET status = 'cancelled', error = COALESCE(error, ?2),
                 finished_at_ms = COALESCE(finished_at_ms, ?3)
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![job_id, reason, now],
        )?;
        return Ok(());
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
