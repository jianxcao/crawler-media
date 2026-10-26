//! Presence changes retire work, while retaining all file and playback facts.

use rusqlite::{Connection, params};

use crate::StoreError;

pub(super) fn cancel_missing_probe_work(
    conn: &Connection,
    ledger_id: &str,
) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM ledger_probe_obligations WHERE ledger_id = ?1",
        [ledger_id],
    )?;
    let jobs: Vec<(String, String)> = conn.prepare(
        "SELECT DISTINCT j.id, j.kind FROM probe_jobs j JOIN probe_job_units u ON u.job_id = j.id
         WHERE u.ledger_id = ?1 AND j.status IN ('queued', 'running')",
    )?.query_map([ledger_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (job_id, kind) in jobs {
        let changed = if kind == "marker_refresh" {
            // A refresh publishes one atomic season snapshot. Losing any member
            // cancels pending work for that snapshot, including its other files.
            let changed = conn.execute(
                "UPDATE probe_job_units SET status = 'cancelled', error = 'file_deleted'
                 WHERE job_id = ?1 AND status IN ('queued', 'running')",
                [&job_id],
            )?;
            conn.execute(
                "UPDATE probe_jobs SET status = 'cancelled', error = 'file_deleted', finished_at_ms = ?2
                 WHERE id = ?1 AND status IN ('queued', 'running')",
                params![job_id, crate::unix_now() * 1000],
            )?;
            changed
        } else {
            conn.execute(
                "UPDATE probe_job_units SET status = 'cancelled', error = 'file_deleted'
                 WHERE job_id = ?1 AND ledger_id = ?2 AND status IN ('queued', 'running')",
                params![job_id, ledger_id],
            )?
        };
        crate::ledger_cleanup::resync_probe_job(conn, &job_id)?;
        tracing::info!(
            job_id,
            ledger_id,
            cancelled_units = changed,
            "源文件缺失，已取消待处理探测工作"
        );
    }
    Ok(())
}

pub(super) fn restore_probe_obligation(
    conn: &Connection,
    ledger_id: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO ledger_probe_obligations (ledger_id, created_at_ms, reason)
         VALUES (?1, ?2, 'source_restored') ON CONFLICT(ledger_id) DO NOTHING",
        params![ledger_id, now_ms],
    )?;
    tracing::info!(ledger_id, "源文件恢复，已登记持久探测义务");
    Ok(())
}

/// One-time upgrade for missing rows recorded before presence retired probe work.
pub(crate) fn repair_missing_probe_work(conn: &Connection) -> Result<(), StoreError> {
    let ids: Vec<String> = conn
        .prepare(
            "SELECT DISTINCT l.id FROM ledger l JOIN probe_job_units u ON u.ledger_id = l.id
         JOIN probe_jobs j ON j.id = u.job_id
         WHERE l.missing_at IS NOT NULL AND j.status IN ('queued', 'running')",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM ledger_probe_obligations WHERE ledger_id IN
         (SELECT id FROM ledger WHERE missing_at IS NOT NULL)",
        [],
    )?;
    for id in &ids {
        cancel_missing_probe_work(&tx, id)?;
    }
    tx.commit()?;
    if !ids.is_empty() {
        tracing::info!(
            missing_sources = ids.len(),
            "已取消旧库中缺失源文件的探测任务"
        );
    }
    Ok(())
}
