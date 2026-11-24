//! Read-only queue summary and bulk cancellation of queued probe work.
use std::collections::{BTreeMap, BTreeSet, HashSet};

use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError, counters, now_ms};

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ProbeLibraryQueueSummary {
    pub library_id: String,
    pub library_name: String,
    pub queued_count: usize,
    pub running_count: usize,
    pub obligation_count: usize,
    pub total_pending: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ProbeQueueSummary {
    pub queued_metadata: usize,
    pub running_metadata: usize,
    pub queued_fingerprint: usize,
    pub running_fingerprint: usize,
    pub pending_obligations: usize,
    pub total_pending: usize,
    pub libraries: Vec<ProbeLibraryQueueSummary>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeCancelSummary {
    pub cancelled_units: usize,
    pub cleared_obligations: usize,
    pub cancelled_file_ids: Vec<String>,
}

#[derive(Default)]
struct LibraryAccumulator {
    name: String,
    sort_order: i64,
    queued_count: usize,
    running_count: usize,
    obligation_count: usize,
}

impl Store {
    pub fn probe_queue_summary(&self) -> Result<ProbeQueueSummary, StoreError> {
        self.with_library_read(|db| db.probe_queue_summary())
    }

    pub fn cancel_queued_probe_work(
        &self,
        library_id: Option<&str>,
        reason: &str,
    ) -> Result<ProbeCancelSummary, StoreError> {
        self.with_library_write(|db| db.cancel_queued_probe_work(library_id, reason))
    }
}

impl crate::LibraryConnection<'_> {
    pub fn probe_queue_summary(&self) -> Result<ProbeQueueSummary, StoreError> {
        let mut summary = ProbeQueueSummary::default();
        let mut per_lib: BTreeMap<String, LibraryAccumulator> = BTreeMap::new();
        accumulate_active_units(self.library, &mut summary, &mut per_lib)?;
        accumulate_pending_obligations(self.library, &mut summary, &mut per_lib)?;
        let mut libraries: Vec<(i64, ProbeLibraryQueueSummary)> = per_lib
            .into_iter()
            .filter_map(|(library_id, acc)| {
                let total_pending = acc.queued_count + acc.running_count + acc.obligation_count;
                (total_pending > 0).then_some((
                    acc.sort_order,
                    ProbeLibraryQueueSummary {
                        library_id,
                        library_name: acc.name,
                        queued_count: acc.queued_count,
                        running_count: acc.running_count,
                        obligation_count: acc.obligation_count,
                        total_pending,
                    },
                ))
            })
            .collect();
        libraries.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.library_name.cmp(&right.1.library_name))
                .then_with(|| left.1.library_id.cmp(&right.1.library_id))
        });
        summary.libraries = libraries.into_iter().map(|(_, item)| item).collect();
        summary.total_pending = summary.queued_metadata
            + summary.running_metadata
            + summary.queued_fingerprint
            + summary.running_fingerprint
            + summary.pending_obligations;
        Ok(summary)
    }

    pub fn cancel_queued_probe_work(
        &self,
        library_id: Option<&str>,
        reason: &str,
    ) -> Result<ProbeCancelSummary, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let targets = select_queued_units(&tx, library_id)?;
        let mut cancelled_file_ids = Vec::with_capacity(targets.len());
        let mut cancelled_seen = BTreeSet::new();
        let mut affected_jobs = BTreeSet::new();
        let mut analysis_targets = Vec::new();
        {
            let mut update = tx.prepare(
                "UPDATE probe_job_units SET status = 'cancelled', error = ?3
                 WHERE job_id = ?1 AND file_id = ?2 AND status = 'queued'",
            )?;
            for (job_id, file_id, stage) in targets {
                if update.execute(params![job_id, file_id, reason])? > 0 {
                    if stage == "analysis" {
                        analysis_targets.push((job_id.clone(), file_id.clone()));
                    }
                    affected_jobs.insert(job_id);
                    if cancelled_seen.insert(file_id.clone()) {
                        cancelled_file_ids.push(file_id);
                    }
                }
            }
        }
        for (job_id, file_id) in analysis_targets {
            crate::probe_analysis::transfer_reconciliation_for_cancel_scope(
                &tx, &job_id, &file_id, library_id,
            )?;
        }
        let residual = select_queued_units(&tx, library_id)?;
        if !residual.is_empty() {
            let mut update = tx.prepare(
                "UPDATE probe_job_units SET status = 'cancelled', error = ?3
                 WHERE job_id = ?1 AND file_id = ?2 AND status = 'queued'",
            )?;
            for (job_id, file_id, _) in residual {
                if update.execute(params![job_id, file_id, reason])? > 0 {
                    affected_jobs.insert(job_id);
                    if cancelled_seen.insert(file_id.clone()) {
                        cancelled_file_ids.push(file_id);
                    }
                }
            }
        }
        let now = now_ms();
        for job_id in affected_jobs {
            reconcile_cancelled_job(&tx, &job_id, reason, now)?;
        }
        let cleared_obligations = match library_id {
            Some(lib_id) => tx.execute(
                "DELETE FROM file_probe_obligations
                 WHERE EXISTS (
                     SELECT 1 FROM library_files f
                     WHERE f.file_id = file_probe_obligations.file_id AND f.library_id = ?1
                 )",
                [lib_id],
            )?,
            None => tx.execute("DELETE FROM file_probe_obligations", [])?,
        };
        tx.commit()?;
        Ok(ProbeCancelSummary {
            cancelled_units: cancelled_file_ids.len(),
            cleared_obligations,
            cancelled_file_ids,
        })
    }

    pub fn with_write_tx<T, E>(
        &self,
        map_err: impl Fn(StoreError) -> E,
        work: impl FnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, E> {
        if self.library.is_autocommit() {
            let tx = crate::write_tx(self.library).map_err(|err| map_err(err.into()))?;
            let outcome = work(self)?;
            tx.commit().map_err(|err| map_err(err.into()))?;
            return Ok(outcome);
        }
        static NEXT_SAVEPOINT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_SAVEPOINT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("sp_write_tx_{id}");
        self.library
            .execute_batch(&format!("SAVEPOINT {name}"))
            .map_err(|err| map_err(err.into()))?;
        match work(self) {
            Ok(outcome) => {
                self.library
                    .execute_batch(&format!("RELEASE SAVEPOINT {name}"))
                    .map_err(|err| map_err(err.into()))?;
                Ok(outcome)
            }
            Err(err) => {
                if !self.library.is_autocommit()
                    && let Err(rb_err) = self.library.execute_batch(&format!(
                        "ROLLBACK TO SAVEPOINT {name}; RELEASE SAVEPOINT {name}"
                    ))
                {
                    return Err(map_err(rb_err.into()));
                }
                Err(err)
            }
        }
    }

    /// Cancel unstarted `status = 'queued'` units on `ledger_ids` whose `job_id`
    /// is not in `exempt_job_ids`, allowing an explicit refresh while paused to
    /// supersede dormant background work without touching running or exempt jobs.
    ///
    /// Returns `(cancelled_job_id, file_id)` pairs in cancellation order.
    pub fn cancel_dormant_queued_units_for_ledgers(
        &self,
        ledger_ids: &[String],
        exempt_job_ids: &HashSet<String>,
        reason: &str,
    ) -> Result<Vec<(String, String)>, StoreError> {
        if ledger_ids.is_empty() {
            return Ok(Vec::new());
        }
        let tx = crate::write_tx(self.library)?;
        let ledger_json = serde_json::to_string(ledger_ids)?;
        let targets = select_non_exempt_queued_for_ledgers(&tx, &ledger_json, exempt_job_ids)?;
        if targets.is_empty() {
            tx.commit()?;
            return Ok(Vec::new());
        }
        let mut cancelled_units = Vec::with_capacity(targets.len());
        let mut seen_pairs = BTreeSet::new();
        let mut affected_jobs = BTreeSet::new();
        let mut analysis_targets = Vec::new();
        cancel_selected_targets(
            &tx,
            targets,
            reason,
            &mut affected_jobs,
            &mut seen_pairs,
            &mut cancelled_units,
            Some(&mut analysis_targets),
        )?;
        for (job_id, file_id) in analysis_targets {
            crate::probe_analysis::transfer_reconciliation(&tx, &job_id, &file_id)?;
        }
        let residual = select_non_exempt_queued_for_ledgers(&tx, &ledger_json, exempt_job_ids)?;
        if !residual.is_empty() {
            cancel_selected_targets(
                &tx,
                residual,
                reason,
                &mut affected_jobs,
                &mut seen_pairs,
                &mut cancelled_units,
                None,
            )?;
        }
        let now = now_ms();
        for job_id in affected_jobs {
            reconcile_cancelled_job(&tx, &job_id, reason, now)?;
        }
        tx.commit()?;
        Ok(cancelled_units)
    }
}

fn accumulate_active_units(
    conn: &rusqlite::Connection,
    summary: &mut ProbeQueueSummary,
    per_lib: &mut BTreeMap<String, LibraryAccumulator>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT l.library_id, l.name, l.sort_order, u.stage, u.status, COUNT(*)
         FROM probe_job_units u
         JOIN probe_jobs j ON j.id = u.job_id
         JOIN library_files f ON f.file_id = u.file_id
         JOIN libraries l ON l.library_id = f.library_id
         WHERE u.status IN ('queued', 'running')
           AND j.status IN ('queued', 'running')
           AND f.missing_at IS NULL
           AND l.lifecycle_state = 'ready'
           AND NOT EXISTS (SELECT 1 FROM library_removals r WHERE r.library_id = l.library_id)
         GROUP BY l.library_id, l.name, l.sort_order, u.stage, u.status",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?.max(0) as usize,
        ))
    })?;
    for row in rows {
        let (lib_id, lib_name, sort_order, stage, status, count) = row?;
        let entry = per_lib.entry(lib_id).or_default();
        entry.name = lib_name;
        entry.sort_order = sort_order;
        match (stage.as_str(), status.as_str()) {
            ("metadata", "queued") => {
                summary.queued_metadata += count;
                entry.queued_count += count;
            }
            ("metadata", "running") => {
                summary.running_metadata += count;
                entry.running_count += count;
            }
            (_, "queued") => {
                summary.queued_fingerprint += count;
                entry.queued_count += count;
            }
            (_, "running") => {
                summary.running_fingerprint += count;
                entry.running_count += count;
            }
            _ => {}
        }
    }
    Ok(())
}

fn accumulate_pending_obligations(
    conn: &rusqlite::Connection,
    summary: &mut ProbeQueueSummary,
    per_lib: &mut BTreeMap<String, LibraryAccumulator>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT l.library_id, l.name, l.sort_order, COUNT(*)
         FROM file_probe_obligations o
         JOIN library_files f ON f.file_id = o.file_id
         JOIN libraries l ON l.library_id = f.library_id
         WHERE f.missing_at IS NULL
           AND l.lifecycle_state = 'ready'
           AND NOT EXISTS (SELECT 1 FROM library_removals r WHERE r.library_id = l.library_id)
           AND NOT EXISTS (
               SELECT 1 FROM probe_job_units u
               JOIN probe_jobs j ON j.id = u.job_id
               WHERE u.file_id = o.file_id
                 AND u.status IN ('queued', 'running')
                 AND j.status IN ('queued', 'running')
           )
         GROUP BY l.library_id, l.name, l.sort_order",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?.max(0) as usize,
        ))
    })?;
    for row in rows {
        let (lib_id, lib_name, sort_order, count) = row?;
        if count == 0 {
            continue;
        }
        summary.pending_obligations += count;
        let entry = per_lib.entry(lib_id).or_default();
        entry.name = lib_name;
        entry.sort_order = sort_order;
        entry.obligation_count += count;
    }
    Ok(())
}

fn select_queued_units(
    tx: &rusqlite::Connection,
    library_id: Option<&str>,
) -> Result<Vec<(String, String, String)>, StoreError> {
    match library_id {
        Some(lib_id) => {
            let mut stmt = tx.prepare(
                "SELECT u.job_id, u.file_id, u.stage
                 FROM probe_job_units u
                 JOIN probe_jobs j ON j.id = u.job_id
                 WHERE u.status = 'queued'
                   AND j.status IN ('queued', 'running')
                   AND EXISTS (
                       SELECT 1 FROM library_files f
                       WHERE f.file_id = u.file_id AND f.library_id = ?1
                   )",
            )?;
            let rows =
                stmt.query_map([lib_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        }
        None => {
            let mut stmt = tx.prepare(
                "SELECT u.job_id, u.file_id, u.stage
                 FROM probe_job_units u
                 JOIN probe_jobs j ON j.id = u.job_id
                 WHERE u.status = 'queued'
                   AND j.status IN ('queued', 'running')",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        }
    }
}

fn reconcile_cancelled_job(
    tx: &rusqlite::Connection,
    job_id: &str,
    reason: &str,
    now: i64,
) -> Result<(), StoreError> {
    if crate::probe_analysis::fail_incomplete_marker_refresh_in_tx(tx, job_id, now)? {
        return Ok(());
    }
    counters::reconcile_counts_in_transaction(tx, job_id)?;
    let job_row: Option<(String, usize, usize, usize, usize)> = tx
        .query_row(
            "SELECT kind, total, completed, failed, partial
             FROM probe_jobs WHERE id = ?1 AND status IN ('queued', 'running')",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((kind, total, completed, failed, partial)) = job_row else {
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
    let Some(terminal) =
        super::probe_job_terminal_status(&kind, total, completed, failed, partial, cancelled)
    else {
        return Ok(());
    };
    tx.execute(
        "UPDATE probe_jobs
         SET status = ?2,
             finished_at_ms = COALESCE(finished_at_ms, ?3),
             error = COALESCE(error, ?4)
         WHERE id = ?1 AND status IN ('queued', 'running')",
        params![job_id, terminal, now, reason],
    )?;
    Ok(())
}

fn select_non_exempt_queued_for_ledgers(
    tx: &rusqlite::Connection,
    ledger_json: &str,
    exempt_job_ids: &HashSet<String>,
) -> Result<Vec<(String, String, String)>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT u.job_id, u.file_id, u.stage
         FROM probe_job_units u
         WHERE u.file_id IN (SELECT value FROM json_each(?1))
           AND u.status = 'queued'",
    )?;
    let rows = stmt.query_map([ledger_json], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (job_id, file_id, stage) = row?;
        if !exempt_job_ids.contains(&job_id) {
            out.push((job_id, file_id, stage));
        }
    }
    Ok(out)
}

fn cancel_selected_targets(
    tx: &rusqlite::Connection,
    targets: Vec<(String, String, String)>,
    reason: &str,
    affected_jobs: &mut BTreeSet<String>,
    seen_pairs: &mut BTreeSet<(String, String)>,
    cancelled_units: &mut Vec<(String, String)>,
    mut analysis_targets: Option<&mut Vec<(String, String)>>,
) -> Result<(), StoreError> {
    let mut update = tx.prepare(
        "UPDATE probe_job_units SET status = 'cancelled', error = ?3
         WHERE job_id = ?1 AND file_id = ?2 AND status = 'queued'",
    )?;
    for (job_id, file_id, stage) in targets {
        if update.execute(params![job_id, file_id, reason])? > 0 {
            if stage == "analysis"
                && let Some(list) = analysis_targets.as_deref_mut()
            {
                list.push((job_id.clone(), file_id.clone()));
            }
            affected_jobs.insert(job_id.clone());
            let pair = (job_id, file_id);
            if seen_pairs.insert(pair.clone()) {
                cancelled_units.push(pair);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::{ProbeDispatchStage, ProbeJobUnitSpec, Store};

    #[test]
    fn cancel_dormant_queued_units_returns_job_and_file_pairs_while_preserving_exempt_and_running()
    {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        for (job_id, file_id) in [
            ("job-dormant", "file-1"),
            ("job-exempt", "file-2"),
            ("job-running", "file-3"),
        ] {
            assert!(
                store
                    .create_probe_job(
                        job_id,
                        "media_probe",
                        "media-1",
                        None,
                        &format!("ledger:{file_id}"),
                        &[ProbeJobUnitSpec {
                            ledger_id: file_id,
                            kind: "movie",
                            force_fingerprint: false,
                            reuse_fingerprint_cache: true,
                            overwrite_markers: false,
                            reuse_media_info_cache: true,
                        }],
                    )
                    .unwrap()
            );
        }
        assert!(
            store
                .start_probe_unit("job-running", "file-3", ProbeDispatchStage::Metadata)
                .unwrap()
        );
        let exempt = HashSet::from(["job-exempt".to_string()]);
        let target_ids = vec![
            "file-1".to_string(),
            "file-2".to_string(),
            "file-3".to_string(),
        ];
        let cancelled = store
            .with_library_write(|db| {
                db.cancel_dormant_queued_units_for_ledgers(&target_ids, &exempt, "superseded")
            })
            .unwrap();
        assert_eq!(
            cancelled,
            vec![("job-dormant".to_string(), "file-1".to_string())]
        );
        assert_eq!(
            store.get_probe_job("job-dormant").unwrap().unwrap().status,
            "cancelled"
        );
        assert!(store.is_probe_unit_active("job-exempt", "file-2").unwrap());
        assert!(store.is_probe_unit_active("job-running", "file-3").unwrap());
    }

    #[test]
    fn with_write_tx_rolls_back_nested_mutations_when_borrowed_inside_library_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        assert!(
            store
                .create_probe_job(
                    "job-dormant",
                    "media_probe",
                    "media-1",
                    None,
                    "ledger:file-1",
                    &[ProbeJobUnitSpec {
                        ledger_id: "file-1",
                        kind: "movie",
                        force_fingerprint: false,
                        reuse_fingerprint_cache: true,
                        overwrite_markers: false,
                        reuse_media_info_cache: true,
                    }],
                )
                .unwrap()
        );
        let target_ids = vec!["file-1".to_string()];
        let exempt = HashSet::new();
        store
            .library_write(|db| {
                let nested: Result<(), crate::StoreError> =
                    db.with_write_tx(std::convert::identity, |db| {
                        let cancelled = db.cancel_dormant_queued_units_for_ledgers(
                            &target_ids,
                            &exempt,
                            "superseded_by_manual_probe",
                        )?;
                        assert_eq!(
                            cancelled,
                            vec![("job-dormant".to_string(), "file-1".to_string())]
                        );
                        Err(crate::StoreError::Invalid("conflict".into()))
                    });
                assert!(nested.is_err());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.get_probe_job("job-dormant").unwrap().unwrap().status,
            "queued",
            "aborted nested with_write_tx must roll back dormant unit cancellation"
        );
        assert!(store.is_probe_unit_active("job-dormant", "file-1").unwrap());
    }

    fn seed_cross_lib_marker_refresh(
        dir: &std::path::Path,
        store: &Store,
    ) -> (String, Vec<String>) {
        use crate::test_support::{FixtureFile, publish_fixture_file};
        use domain::{Media, MediaId, MediaKind};

        let root_a = dir.join("tv-a");
        let root_b = dir.join("tv-b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "TV-A",
                &[root_a.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let lib_b = store
            .create_library(
                MediaKind::Tv,
                "TV-B",
                &[root_b.to_str().unwrap()],
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
                title: "Refresh Cancel Show".into(),
                year: Some(2026),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let ids: Vec<String> = [(1, &root_a), (2, &root_a), (3, &root_b)]
            .into_iter()
            .map(|(ep, root)| {
                let p = root.join(format!("S01E{ep:02}.mkv"));
                std::fs::write(&p, b"tv").unwrap();
                publish_fixture_file(
                    store,
                    &FixtureFile {
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
        let specs: Vec<_> = ids
            .iter()
            .map(|lid| ProbeJobUnitSpec {
                ledger_id: lid,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: false,
            })
            .collect();
        assert!(
            store
                .create_probe_job(
                    "refresh-cross-lib",
                    "marker_refresh",
                    &media_id.to_string(),
                    Some(1),
                    "marker:cross-lib",
                    &specs,
                )
                .unwrap()
        );
        (lib_b.id, ids)
    }

    #[test]
    fn cancel_queued_probe_work_fails_marker_refresh_when_prior_unit_partial_and_only_analysis_remains()
     {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let (lib_b_id, ids) = seed_cross_lib_marker_refresh(dir.path(), &store);
        store
            .with_library_write(|db| {
                db.library.execute(
                    "UPDATE probe_job_units SET stage = 'analysis', status = 'queued'
                     WHERE job_id = 'refresh-cross-lib' AND file_id = ?1",
                    [&ids[0]],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            store
                .start_probe_unit("refresh-cross-lib", &ids[1], ProbeDispatchStage::Metadata)
                .unwrap()
        );
        store
            .finish_probe_unit_with_status(
                "refresh-cross-lib",
                &ids[1],
                "partial",
                Some("timeout"),
                Some("tail unavailable"),
            )
            .unwrap();

        let summary = store
            .library_write(|db| db.cancel_queued_probe_work(Some(&lib_b_id), "manual_cancel"))
            .unwrap();
        assert_eq!(summary.cancelled_units, 1);
        let job = store.get_probe_job("refresh-cross-lib").unwrap().unwrap();
        assert_eq!(
            job.status, "failed",
            "cancelling last non-analysis unit when a prior unit was partial must fail marker_refresh"
        );
        assert!(
            store
                .active_probe_unit_for_ledger(&ids[0])
                .unwrap()
                .is_none()
        );
    }
}
