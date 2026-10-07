use domain::JobId;
use rusqlite::{Connection, params};

use super::get_job;
use crate::rows::{JOB_COLS, map_job};
use crate::types::{Job, JobError, JobStatus};

const MAX_ATTEMPTS: u32 = 5;
const RETRY_BASE_SECS: i64 = 30;
const RETRY_MAX_SECS: i64 = 300;

pub fn finish_claimed(
    conn: &Connection,
    claimed: &Job,
    now: i64,
    status: JobStatus,
) -> Result<Option<Job>, JobError> {
    let changed = conn.execute(
        "UPDATE jobs SET status = ?1, finished_at = ?2, error = NULL
         WHERE id = ?3 AND status = 'running' AND attempt = ?4 AND started_at = ?5",
        params![
            status.as_str(),
            now,
            claimed.id.to_string(),
            claimed.attempt,
            claimed.started_at
        ],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    let job = get_job(conn, claimed.id)?.ok_or(JobError::UnknownJob(claimed.id))?;
    if matches!(
        status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        crate::schedule::spawn_follow_up(conn, &job, now)?;
    }
    Ok(Some(job))
}

fn compute_fail_claimed_status(
    conn: &Connection,
    claimed: &Job,
    attempt: u32,
) -> Result<(JobStatus, bool), JobError> {
    let has_queued_follow_up = if attempt < MAX_ATTEMPTS {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE def_id = ?1 AND status = 'queued' AND id != ?2)",
            params![claimed.def_id.map(|id| id.to_string()), claimed.id.to_string()],
            |row| row.get::<_, bool>(0),
        )?
    } else {
        false
    };
    let def_enabled = if let Some(def_id) = claimed.def_id {
        conn.query_row(
            "SELECT enabled FROM job_defs WHERE id = ?1",
            params![def_id.to_string()],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(true)
    } else {
        true
    };
    let status = if attempt >= MAX_ATTEMPTS || has_queued_follow_up || !def_enabled {
        JobStatus::Failed
    } else {
        JobStatus::Queued
    };
    Ok((status, has_queued_follow_up))
}

pub fn fail_claimed(
    conn: &Connection,
    claimed: &Job,
    now: i64,
    error: &str,
) -> Result<Option<Job>, JobError> {
    let attempt = claimed.attempt.saturating_add(1);
    let (status, has_queued_follow_up) = compute_fail_claimed_status(conn, claimed, attempt)?;
    let retry_delay = RETRY_BASE_SECS
        .saturating_mul(1_i64 << attempt.saturating_sub(1).min(4))
        .min(RETRY_MAX_SECS);
    let run_after = now.saturating_add(retry_delay);
    let changed = conn.execute(
        "UPDATE jobs SET status = ?1, attempt = ?2, error = ?3, run_after = ?4,
                finished_at = CASE WHEN ?1 = 'failed' THEN ?5 ELSE NULL END,
                started_at = CASE WHEN ?1 = 'queued' THEN NULL ELSE started_at END
         WHERE id = ?6 AND status = 'running' AND attempt = ?7 AND started_at = ?8",
        params![
            status.as_str(),
            attempt,
            error,
            run_after,
            now,
            claimed.id.to_string(),
            claimed.attempt,
            claimed.started_at
        ],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    let updated = get_job(conn, claimed.id)?.ok_or(JobError::UnknownJob(claimed.id))?;
    if has_queued_follow_up {
        tracing::warn!(job_id = %updated.id, kind = updated.kind.as_str(), attempt, error = %error, "任务失败，已有待执行的后续任务将继续处理");
        crate::schedule::spawn_follow_up(conn, &updated, now)?;
    } else if attempt >= MAX_ATTEMPTS {
        tracing::error!(job_id = %updated.id, kind = updated.kind.as_str(), attempt, error = %error, "任务达到最大重试次数，标记为失败");
        crate::schedule::spawn_follow_up(conn, &updated, now)?;
    } else {
        tracing::warn!(job_id = %updated.id, kind = updated.kind.as_str(), attempt, error = %error, "任务执行失败，将重试");
    }
    Ok(Some(updated))
}

pub fn fail(conn: &Connection, id: JobId, now: i64, error: &str) -> Result<Job, JobError> {
    let claimed = get_job(conn, id)?.ok_or(JobError::UnknownJob(id))?;
    fail_claimed(conn, &claimed, now, error)?.ok_or(JobError::UnknownJob(id))
}

pub fn recover(conn: &Connection, now: i64) -> Result<Vec<Job>, JobError> {
    recover_except(conn, now, &[])
}

/// Recover timed-out jobs except attempts still executing in this process.
/// A process restart naturally clears the in-memory active set, allowing
/// abandoned database claims to be recovered as usual.
pub fn recover_except(conn: &Connection, now: i64, active: &[Job]) -> Result<Vec<Job>, JobError> {
    let mut recovered = Vec::new();
    for job in list_running(conn)? {
        if active.iter().any(|attempt| same_attempt(attempt, &job)) {
            continue;
        }
        let timeout = i64::from(job.timeout_secs.unwrap_or(30));
        let started = job.started_at.unwrap_or(job.run_after);
        if now - started < timeout {
            continue;
        }
        recovered.push(fail(conn, job.id, now, "timed out")?);
    }
    Ok(recovered)
}

fn same_attempt(left: &Job, right: &Job) -> bool {
    left.id == right.id && left.attempt == right.attempt && left.started_at == right.started_at
}

fn list_running(conn: &Connection) -> Result<Vec<Job>, JobError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLS} FROM jobs WHERE status = 'running'"
    ))?;
    let rows = stmt.query_map([], map_job)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
