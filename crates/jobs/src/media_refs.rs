use crate::{Job, JobDef, JobError, JobIdentity, JobStatus};
use domain::MediaId;
use rusqlite::{Connection, params};

/// Merge exact CatalogRefresh identities atomically. A running loser keeps its
/// attempt and outcome; the durable Media convergence loop retries after it ends.
pub fn rewrite_media_refs(conn: &Connection, from: &str, to: &str) -> Result<usize, JobError> {
    let parse = |raw: &str| {
        raw.parse::<MediaId>()
            .map_err(|_| JobError::Identity(raw.into()))
    };
    let from = parse(from)?;
    let to = parse(to)?;
    if from == to {
        return Ok(0);
    }
    let source = JobIdentity::catalog_refresh(from);
    let target = JobIdentity::catalog_refresh(to);
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let pending = live_jobs(&tx, source)?;
    if pending.iter().any(|job| job.status == JobStatus::Running) {
        return Err(JobError::Conflict(format!(
            "CatalogRefresh {from} still running; retry convergence"
        )));
    }
    let loser = crate::schedule::def_for_identity(&tx, source)?;
    let survivor = crate::schedule::def_for_identity(&tx, target)?;
    if let Some(def) = &loser {
        source.validate_payload(&def.payload)?;
    }
    for job in &pending {
        source.validate_payload(&job.payload)?;
    }
    let changed = if let (Some(loser), Some(survivor)) = (&loser, &survivor) {
        merge_definitions(&tx, source, loser, survivor, !pending.is_empty(), to)?
    } else {
        retarget_pending(&tx, loser.as_ref(), &pending, from, to)?
    };
    tx.commit()?;
    Ok(changed)
}

fn live_jobs(conn: &Connection, identity: JobIdentity) -> Result<Vec<Job>, JobError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM jobs WHERE kind=?1 AND scope_key=?2 AND status IN ('queued','running')",
        crate::rows::JOB_COLS
    ))?;
    let rows = stmt.query_map(
        params![identity.kind.as_str(), identity.scope.to_string()],
        crate::rows::map_job,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn merge_definitions(
    conn: &Connection,
    source: JobIdentity,
    loser: &JobDef,
    survivor: &JobDef,
    pending: bool,
    to: MediaId,
) -> Result<usize, JobError> {
    // The survivor's arguments and settings remain authoritative. Preserve source
    // rows as explicit coalesced history and represent pending work on survivor.
    let mut changed = 0;
    if pending {
        coalesce_child(conn, survivor)?;
        changed+=conn.execute("UPDATE jobs SET status='cancelled', finished_at=strftime('%s','now'), error=?3 WHERE kind=?1 AND scope_key=?2 AND status IN ('queued','running')",
            params![source.kind.as_str(),source.scope.to_string(),format!("coalesced into Media {to}")])?;
    }
    changed += conn.execute("DELETE FROM job_defs WHERE id=?1", [loser.id.to_string()])?;
    Ok(changed)
}

fn retarget_pending(
    conn: &Connection,
    loser: Option<&JobDef>,
    pending: &[Job],
    from: MediaId,
    to: MediaId,
) -> Result<usize, JobError> {
    let scope = crate::JobScope::Media(to).to_string();
    let mut changed = 0;
    if let Some(loser) = loser {
        changed += conn.execute(
            "UPDATE job_defs SET scope_key=?1,payload=?2,concurrency_key=?3 WHERE id=?4",
            params![
                scope,
                retarget(&loser.payload, to)?,
                retarget_key(loser.concurrency_key.as_deref(), from, to),
                loser.id.to_string()
            ],
        )?;
    }
    for job in pending {
        changed+=conn.execute("UPDATE jobs SET scope_key=?1,payload=?2,concurrency_key=?3 WHERE id=?4 AND status='queued'",
            params![scope,retarget(&job.payload,to)?,retarget_key(job.concurrency_key.as_deref(),from,to),job.id.to_string()])?;
    }
    Ok(changed)
}

fn coalesce_child(conn: &Connection, survivor: &JobDef) -> Result<(), JobError> {
    if let Some(child) = crate::schedule::get_live_child(conn, survivor.id)?
        && child.status == JobStatus::Queued
    {
        conn.execute(
            "UPDATE jobs SET run_after=0 WHERE id=?1",
            [child.id.to_string()],
        )?;
        return Ok(());
    }
    // A disabled survivor retains accepted pending work until explicitly enabled.
    crate::store::insert_job(
        conn,
        Some(survivor.id),
        JobIdentity {
            kind: survivor.kind,
            scope: survivor.scope,
        },
        &survivor.payload,
        0,
        survivor.concurrency_key.as_deref(),
        survivor.timeout_secs,
    )?;
    Ok(())
}
fn retarget(payload: &str, to: MediaId) -> Result<String, JobError> {
    let mut value: serde_json::Value = serde_json::from_str(payload)?;
    value["media_id"] = serde_json::Value::String(to.to_string());
    Ok(serde_json::to_string(&value)?)
}
fn retarget_key(key: Option<&str>, from: MediaId, to: MediaId) -> Option<String> {
    key.map(|key| {
        if key == format!("catalog:{from}") {
            format!("catalog:{to}")
        } else {
            key.into()
        }
    })
}
