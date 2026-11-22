//! Read-only queue summary and bulk cancellation of queued probe work.
use std::collections::{BTreeMap, BTreeSet};

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
        let mut affected_jobs = BTreeSet::new();
        {
            let mut update = tx.prepare(
                "UPDATE probe_job_units SET status = 'cancelled', error = ?3
                 WHERE job_id = ?1 AND file_id = ?2 AND status = 'queued'",
            )?;
            for (job_id, file_id, stage) in targets {
                if update.execute(params![job_id, file_id, reason])? > 0 {
                    if stage == "analysis" {
                        crate::probe_analysis::transfer_reconciliation(&tx, &job_id, &file_id)?;
                    }
                    affected_jobs.insert(job_id);
                    cancelled_file_ids.push(file_id);
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
    counters::reconcile_counts_in_transaction(tx, job_id)?;
    let job_row: Option<(usize, usize, usize, usize)> = tx
        .query_row(
            "SELECT total, completed, failed, partial
             FROM probe_jobs WHERE id = ?1 AND status IN ('queued', 'running')",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((total, completed, failed, partial)) = job_row else {
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
    let terminal = if failed > 0 {
        "failed"
    } else if cancelled > 0 {
        "cancelled"
    } else if partial > 0 {
        "partial"
    } else {
        "succeeded"
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
