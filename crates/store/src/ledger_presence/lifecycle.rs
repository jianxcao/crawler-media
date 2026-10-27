//! Probe lifecycle follows the same committed generation as its presence fact.
use crate::StoreError;
use rusqlite::{Connection, params};

pub(super) fn source_missing(
    tx: &Connection,
    file_id: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    let jobs: Vec<(String, String)> = tx.prepare(
        "SELECT j.id, j.kind FROM probe_jobs j JOIN probe_job_units u ON u.job_id=j.id
         WHERE u.file_id=?1 AND u.status IN ('queued','running') AND j.status IN ('queued','running')"
    )?.query_map([file_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    for (job_id, kind) in jobs {
        if kind == "marker_refresh" {
            tx.execute(
                "UPDATE probe_job_units SET status='cancelled', error='source missing'
                        WHERE job_id=?1 AND status IN ('queued','running')",
                [&job_id],
            )?;
            tx.execute("UPDATE probe_jobs SET status='cancelled', error='source missing', finished_at_ms=?2
                        WHERE id=?1", params![job_id, now_ms])?;
        } else {
            tx.execute(
                "UPDATE probe_job_units SET status='cancelled', error='source missing'
                        WHERE job_id=?1 AND file_id=?2 AND status IN ('queued','running')",
                params![job_id, file_id],
            )?;
        }
        crate::ledger_cleanup::probe_unit_retirement::resync_probe_job(tx, &job_id)?;
    }
    tx.execute(
        "DELETE FROM file_probe_obligations WHERE file_id=?1",
        [file_id],
    )?;
    Ok(())
}

pub(super) fn source_restored(
    tx: &Connection,
    file_id: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO file_probe_obligations(file_id,created_at_ms,reason)
                VALUES(?1,?2,'source_restored') ON CONFLICT(file_id) DO UPDATE SET
                created_at_ms=excluded.created_at_ms, reason=excluded.reason",
        params![file_id, now_ms],
    )?;
    Ok(())
}
