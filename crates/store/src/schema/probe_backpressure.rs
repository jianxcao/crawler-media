//! One-time compensation for probe work cancelled by the old dispatch window.

use rusqlite::Connection;

use crate::StoreError;

pub(super) fn repair_capacity_cancelled_obligations(conn: &Connection) -> Result<(), StoreError> {
    // Keep the historical cancelled job. Recreate its scan obligation only if
    // no later attempt superseded it; ordinary admission evaluates current
    // caches, retry policy and source availability before scheduling any IO.
    let repaired = conn.execute(
        "INSERT OR IGNORE INTO ledger_probe_obligations (ledger_id, created_at_ms, reason)
         SELECT l.id, j.created_at_ms, 'dispatch_window_recovery'
         FROM probe_job_units u
         JOIN probe_jobs j ON j.id = u.job_id
         JOIN ledger l ON l.id = u.ledger_id
         WHERE u.status = 'cancelled' AND u.error = 'dispatch window full'
           AND j.kind = 'media_probe' AND l.missing_at IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM probe_job_units newer
               WHERE newer.ledger_id = u.ledger_id AND newer.rowid > u.rowid
           )",
        [],
    )?;
    if repaired > 0 {
        tracing::warn!(repaired, "已恢复旧版本因派发窗口满而丢失的扫描探测义务");
    }
    Ok(())
}
