use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError, StoredFingerprintSample};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFingerprintAttempt {
    pub attempt_id: String,
    pub job_id: String,
    pub ledger_id: String,
    pub kind: String,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub phase: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub status: String,
    pub error_kind: Option<String>,
    pub metrics_json: String,
}

impl Store {
    pub fn begin_fingerprint_attempt(
        &self,
        attempt: &StoredFingerprintAttempt,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "INSERT INTO fingerprint_attempts (
                 attempt_id, job_id, ledger_id, kind, window_start_ms,
                 window_end_ms, phase, started_at_ms, finished_at_ms,
                 status, error_kind, metrics_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(attempt_id) DO UPDATE SET
                 job_id = excluded.job_id,
                 ledger_id = excluded.ledger_id,
                 kind = excluded.kind,
                 window_start_ms = excluded.window_start_ms,
                 window_end_ms = excluded.window_end_ms,
                 phase = excluded.phase,
                 started_at_ms = excluded.started_at_ms,
                 finished_at_ms = excluded.finished_at_ms,
                 status = excluded.status,
                 error_kind = excluded.error_kind,
                 metrics_json = excluded.metrics_json",
            params![
                attempt.attempt_id,
                attempt.job_id,
                attempt.ledger_id,
                attempt.kind,
                attempt.window_start_ms,
                attempt.window_end_ms,
                attempt.phase,
                attempt.started_at_ms,
                attempt.finished_at_ms,
                attempt.status,
                attempt.error_kind,
                attempt.metrics_json,
            ],
        )?;
        Ok(())
    }

    pub fn complete_fingerprint_attempt(
        &self,
        attempt: &StoredFingerprintAttempt,
        sample: Option<&StoredFingerprintSample>,
    ) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;

        tx.execute(
            "INSERT INTO fingerprint_attempts (
                 attempt_id, job_id, ledger_id, kind, window_start_ms,
                 window_end_ms, phase, started_at_ms, finished_at_ms,
                 status, error_kind, metrics_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(attempt_id) DO UPDATE SET
                 job_id = excluded.job_id,
                 ledger_id = excluded.ledger_id,
                 kind = excluded.kind,
                 window_start_ms = excluded.window_start_ms,
                 window_end_ms = excluded.window_end_ms,
                 phase = excluded.phase,
                 started_at_ms = excluded.started_at_ms,
                 finished_at_ms = excluded.finished_at_ms,
                 status = excluded.status,
                 error_kind = excluded.error_kind,
                 metrics_json = excluded.metrics_json",
            params![
                attempt.attempt_id,
                attempt.job_id,
                attempt.ledger_id,
                attempt.kind,
                attempt.window_start_ms,
                attempt.window_end_ms,
                attempt.phase,
                attempt.started_at_ms,
                attempt.finished_at_ms,
                attempt.status,
                attempt.error_kind,
                attempt.metrics_json,
            ],
        )?;

        if let Some(s) = sample {
            tx.execute(
                "INSERT INTO fingerprint_samples (
                     sample_id, ledger_id, source_version, capture_profile_key,
                     kind, window_start_ms, window_end_ms, pcm_duration_ms,
                     fingerprint_json, captured_job_id, captured_at_ms, metrics_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(ledger_id, source_version, capture_profile_key, kind, window_start_ms, window_end_ms)
                 DO UPDATE SET
                     sample_id = excluded.sample_id,
                     pcm_duration_ms = excluded.pcm_duration_ms,
                     fingerprint_json = excluded.fingerprint_json,
                     captured_job_id = excluded.captured_job_id,
                     captured_at_ms = excluded.captured_at_ms,
                     metrics_json = excluded.metrics_json",
                params![
                    s.sample_id,
                    s.ledger_id,
                    s.source_version,
                    s.capture_profile_key,
                    s.kind,
                    s.window_start_ms,
                    s.window_end_ms,
                    s.pcm_duration_ms,
                    serde_json::to_string(&s.fingerprint)?,
                    s.captured_job_id,
                    s.captured_at_ms,
                    s.metrics_json,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn get_fingerprint_attempt(
        &self,
        attempt_id: &str,
    ) -> Result<Option<StoredFingerprintAttempt>, StoreError> {
        self.library
            .query_row(
                "SELECT attempt_id, job_id, ledger_id, kind, window_start_ms,
                        window_end_ms, phase, started_at_ms, finished_at_ms,
                        status, error_kind, metrics_json
                 FROM fingerprint_attempts WHERE attempt_id = ?1",
                params![attempt_id],
                |row| {
                    Ok(StoredFingerprintAttempt {
                        attempt_id: row.get(0)?,
                        job_id: row.get(1)?,
                        ledger_id: row.get(2)?,
                        kind: row.get(3)?,
                        window_start_ms: row.get(4)?,
                        window_end_ms: row.get(5)?,
                        phase: row.get(6)?,
                        started_at_ms: row.get(7)?,
                        finished_at_ms: row.get(8)?,
                        status: row.get(9)?,
                        error_kind: row.get(10)?,
                        metrics_json: row.get(11)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn recover_interrupted_fingerprint_attempts(
        &self,
        job_id: &str,
    ) -> Result<usize, StoreError> {
        let changed = self.library.execute(
            "UPDATE fingerprint_attempts
             SET status = 'interrupted',
                 metrics_json = json_set(metrics_json, '$.measurement_complete', json('false'))
             WHERE job_id = ?1 AND status = 'running'",
            params![job_id],
        )?;
        Ok(changed)
    }
}
