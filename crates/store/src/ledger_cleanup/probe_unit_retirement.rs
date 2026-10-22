//! Probe work items that outlive their Library file.
//!
//! `probe_job_units` is the *live* work queue while `probe_jobs.total/completed/
//! succeeded/failed` are derived from it. A unit whose file row is gone can
//! never run again, so it is dropped — and the job is resynced instead of being
//! left `running` with a completion predicate it can no longer satisfy.
//!

use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// Remove every probe work item for `file_id` and resync the jobs that owned them.
pub(crate) fn retire_probe_units_for_file(
    conn: &Connection,
    file_id: &str,
) -> Result<(), StoreError> {
    let job_ids: Vec<String> = conn
        .prepare("SELECT DISTINCT job_id FROM probe_job_units WHERE file_id = ?1")?
        .query_map([file_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if job_ids.is_empty() {
        return Ok(());
    }
    conn.execute("DELETE FROM probe_job_units WHERE file_id = ?1", [file_id])?;
    for job_id in &job_ids {
        resync_probe_job(conn, job_id)?;
    }
    Ok(())
}

/// Recompute `probe_jobs` counters from the live units, and terminalize an
/// active job whose remaining work is all terminal instead of leaving it
/// `running` with a completion predicate nothing can satisfy.
///
/// A job with no units left at all is cancelled (`file_deleted`): its whole
/// scope disappeared. A job that still has a genuinely outstanding unit is
/// untouched; one whose remaining units all finished takes the same per-kind
/// terminal state as the normal completion path.
pub(crate) fn resync_probe_job(conn: &Connection, job_id: &str) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE probe_jobs SET
             total = (SELECT COUNT(*) FROM probe_job_units WHERE job_id = ?1),
             completed = (SELECT COUNT(*) FROM probe_job_units WHERE job_id = ?1
                          AND status IN ('succeeded', 'failed', 'cancelled', 'partial')),
             succeeded = (SELECT COUNT(*) FROM probe_job_units
                          WHERE job_id = ?1 AND status = 'succeeded'),
             failed = (SELECT COUNT(*) FROM probe_job_units
                       WHERE job_id = ?1 AND status = 'failed'),
             partial = (SELECT COUNT(*) FROM probe_job_units
                        WHERE job_id = ?1 AND status = 'partial')
         WHERE id = ?1",
        [job_id],
    )?;
    let job: Option<(String, String, i64, i64, i64, i64)> = conn
        .query_row(
            "SELECT kind, status, total, completed, failed,
                    (SELECT COUNT(*) FROM probe_job_units
                     WHERE job_id = probe_jobs.id AND status = 'partial')
             FROM probe_jobs WHERE id = ?1",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((kind, status, total, completed, failed, partial)) = job else {
        return Ok(());
    };
    if status != "queued" && status != "running" {
        return Ok(());
    }
    if total == 0 {
        conn.execute(
            "UPDATE probe_jobs SET status = 'cancelled',
                 error = COALESCE(error, 'file_deleted'),
                 finished_at_ms = COALESCE(finished_at_ms, ?2)
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![job_id, crate::unix_now() * 1000],
        )?;
        return Ok(());
    }
    let cancelled: i64 = conn.query_row(
        "SELECT COUNT(*) FROM probe_job_units WHERE job_id = ?1 AND status = 'cancelled'",
        [job_id],
        |row| row.get(0),
    )?;
    let Some(terminal) = crate::probe_tasks::probe_job_terminal_status(
        &kind,
        total as usize,
        completed as usize,
        failed as usize,
        partial as usize,
        cancelled as usize,
    ) else {
        return Ok(());
    };
    conn.execute(
        "UPDATE probe_jobs SET status = ?2,
             finished_at_ms = COALESCE(finished_at_ms, ?3),
             error = COALESCE((SELECT error FROM probe_job_units
                               WHERE job_id = ?1 AND error IS NOT NULL LIMIT 1), error)
         WHERE id = ?1 AND status IN ('queued', 'running')",
        params![job_id, terminal, crate::unix_now() * 1000],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
