use std::str::FromStr;

use domain::{JobDefId, JobId};

use crate::kinds::{JobKind, Schedule};
use crate::types::{Job, JobDef, JobStatus};

pub const JOB_COLS: &str = "id, def_id, kind, payload, status, attempt, run_after, \
     started_at, finished_at, error, progress, concurrency_key, timeout_secs";

pub fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let kind = JobKind::from_str(&row.get::<_, String>(2)?).map_err(|e| col_err(2, e))?;
    let status = JobStatus::from_str(&row.get::<_, String>(4)?).map_err(|e| col_err(4, e))?;
    Ok(Job {
        id: parse_id(row, 0, JobId::from_str)?,
        def_id: optional_id(row, 1, JobDefId::from_str)?,
        kind,
        payload: row.get(3)?,
        status,
        attempt: row.get::<_, i64>(5)? as u32,
        run_after: row.get(6)?,
        started_at: row.get(7)?,
        finished_at: row.get(8)?,
        error: row.get(9)?,
        progress: row.get(10)?,
        concurrency_key: row.get(11)?,
        timeout_secs: row.get::<_, Option<i64>>(12)?.map(|v| v as u32),
    })
}

pub fn map_def(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobDef> {
    let kind = JobKind::from_str(&row.get::<_, String>(1)?).map_err(|e| col_err(1, e))?;
    let schedule = match row.get::<_, Option<String>>(4)? {
        Some(raw) => Some(Schedule::parse(&raw).map_err(|e| col_err(4, e))?),
        None => None,
    };
    Ok(JobDef {
        id: parse_id(row, 0, JobDefId::from_str)?,
        kind,
        name: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        schedule,
        payload: row.get(5)?,
        timeout_secs: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
        concurrency_key: row.get(7)?,
    })
}

fn parse_id<T, E: std::fmt::Display>(
    row: &rusqlite::Row<'_>,
    idx: usize,
    parse: fn(&str) -> Result<T, E>,
) -> rusqlite::Result<T> {
    let raw: String = row.get(idx)?;
    parse(&raw).map_err(|e| col_err(idx, e.to_string()))
}

fn optional_id<T, E: std::fmt::Display>(
    row: &rusqlite::Row<'_>,
    idx: usize,
    parse: fn(&str) -> Result<T, E>,
) -> rusqlite::Result<Option<T>> {
    let raw: Option<String> = row.get(idx)?;
    match raw {
        Some(raw) => parse(&raw)
            .map(Some)
            .map_err(|e| col_err(idx, e.to_string())),
        None => Ok(None),
    }
}

fn col_err(idx: usize, msg: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        idx,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::other(msg)),
    )
}
