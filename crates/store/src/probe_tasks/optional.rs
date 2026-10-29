use rusqlite::{Transaction, params};

use crate::{Store, StoreError};

impl Store {
    /// Persist the switches and retire affected optional work atomically.
    pub fn set_library_intro_settings(
        &self,
        id: &str,
        detect_intros: bool,
        enable_fingerprint: bool,
    ) -> Result<(), StoreError> {
        self.ensure_library_mutable(id)?;
        let tx = crate::write_tx(&self.library)?;
        let disabling = tx.query_row(
            "SELECT (detect_intros=1 AND ?2=0) OR (enable_fingerprint=1 AND ?3=0)
             FROM libraries WHERE library_id=?1",
            params![id, detect_intros, enable_fingerprint],
            |row| row.get::<_, bool>(0),
        )?;
        tx.execute(
            "UPDATE libraries SET detect_intros = ?1, enable_fingerprint = ?2 WHERE library_id = ?3",
            params![detect_intros, enable_fingerprint, id],
        )?;
        if disabling {
            cancel_optional_library_work(&tx, id)?;
        }
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn file_accepts_optional_work(
    tx: &Transaction<'_>,
    file_id: &str,
    automatic: bool,
) -> Result<bool, StoreError> {
    Ok(tx.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM library_files f JOIN libraries l ON l.library_id=f.library_id
             WHERE f.file_id=?1 AND f.missing_at IS NULL
               AND (?2=0 OR (l.detect_intros=1 AND l.enable_fingerprint=1))
               AND NOT EXISTS (SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id)
         )",
        params![file_id, automatic],
        |row| row.get(0),
    )?)
}

fn cancel_optional_library_work(tx: &Transaction<'_>, id: &str) -> Result<(), StoreError> {
    let jobs = {
        let mut statement = tx.prepare(
            "SELECT DISTINCT j.id FROM probe_jobs j
             JOIN probe_job_units u ON u.job_id=j.id
             JOIN library_files f ON f.file_id=u.file_id
             WHERE f.library_id=?1 AND j.kind IN ('fingerprint_probe','marker_refresh')
               AND j.status IN ('queued','running')
               AND (u.status IN ('queued','running')
                   OR (j.kind='marker_refresh' AND u.status IN ('succeeded','partial')))",
        )?;
        statement
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    tx.execute(
        "UPDATE probe_job_units SET status='cancelled', error='optional_detection_disabled'
         WHERE file_id IN (SELECT file_id FROM library_files WHERE library_id=?1)
           AND job_id IN (SELECT id FROM probe_jobs
               WHERE kind IN ('fingerprint_probe','marker_refresh') AND status IN ('queued','running'))
           AND (status IN ('queued','running')
               OR (status IN ('succeeded','partial') AND job_id IN (
                   SELECT id FROM probe_jobs WHERE kind='marker_refresh')))",
        [id],
    )?;
    for job in jobs {
        super::counters::reconcile_counts_in_transaction(tx, &job)?;
        tx.execute(
            "UPDATE probe_jobs SET status='cancelled', error='optional_detection_disabled', finished_at_ms=?2
             WHERE id=?1 AND status IN ('queued','running')
               AND NOT EXISTS (SELECT 1 FROM probe_job_units
                   WHERE job_id=?1 AND status IN ('queued','running'))",
            params![job, super::now_ms()],
        )?;
    }
    Ok(())
}
