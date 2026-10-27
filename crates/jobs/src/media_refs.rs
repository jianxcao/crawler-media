use rusqlite::params;
use serde_json::Value;

use crate::JobError;

/// Rewrite top-level `media_id` fields in active job payloads from `from` to `to`.
///
/// Modifies `job_defs.payload` and `jobs.payload` (only for `status IN ('queued', 'running')`).
/// Uses `payload LIKE '%'||?||'%'` index acceleration, verifies top-level JSON `media_id`,
/// and runs in a single transaction. Returns the number of updated payloads.
pub fn rewrite_media_refs(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
) -> Result<usize, JobError> {
    if from == to {
        return Ok(0);
    }

    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut updated_count = 0;

    // 1. Process job_defs
    {
        let mut stmt =
            tx.prepare("SELECT id, payload FROM job_defs WHERE payload LIKE '%'||?1||'%'")?;
        let rows = stmt.query_map(params![from], |r| {
            let id: String = r.get(0)?;
            let payload: String = r.get(1)?;
            Ok((id, payload))
        })?;
        let defs: Vec<(String, String)> = rows.collect::<Result<Vec<_>, _>>()?;

        for (id, payload_str) in defs {
            if let Ok(mut val) = serde_json::from_str::<Value>(&payload_str)
                && let Some(obj) = val.as_object_mut()
                && obj.get("media_id").and_then(Value::as_str) == Some(from)
            {
                obj.insert("media_id".to_string(), Value::String(to.to_string()));
                let new_payload = serde_json::to_string(&val)?;
                tx.execute(
                    "UPDATE job_defs SET payload = ?1 WHERE id = ?2",
                    params![new_payload, id],
                )?;
                updated_count += 1;
            }
        }
    }

    // 2. Process jobs with status IN ('queued', 'running')
    {
        let mut stmt = tx.prepare(
            "SELECT id, payload FROM jobs
             WHERE status IN ('queued', 'running') AND payload LIKE '%'||?1||'%'",
        )?;
        let rows = stmt.query_map(params![from], |r| {
            let id: String = r.get(0)?;
            let payload: String = r.get(1)?;
            Ok((id, payload))
        })?;
        let jobs: Vec<(String, String)> = rows.collect::<Result<Vec<_>, _>>()?;

        for (id, payload_str) in jobs {
            if let Ok(mut val) = serde_json::from_str::<Value>(&payload_str)
                && let Some(obj) = val.as_object_mut()
                && obj.get("media_id").and_then(Value::as_str) == Some(from)
            {
                obj.insert("media_id".to_string(), Value::String(to.to_string()));
                let new_payload = serde_json::to_string(&val)?;
                tx.execute(
                    "UPDATE jobs SET payload = ?1 WHERE id = ?2",
                    params![new_payload, id],
                )?;
                updated_count += 1;
            }
        }
    }

    tx.commit()?;
    Ok(updated_count)
}
