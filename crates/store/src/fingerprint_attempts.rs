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
    /// A persisted task must remain active and its matching unit must not be cancelled.
    /// Standalone captures and season members outside the task's scope have no unit row.
    pub fn fingerprint_source_active(
        &self,
        job_id: &str,
        file_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.fingerprint_source_active(job_id, file_id))
    }

    pub fn begin_fingerprint_attempt(
        &self,
        attempt: &StoredFingerprintAttempt,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.begin_fingerprint_attempt(attempt))
    }

    pub fn complete_fingerprint_attempt(
        &self,
        attempt: &StoredFingerprintAttempt,
        sample: Option<&StoredFingerprintSample>,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.complete_fingerprint_attempt(attempt, sample))
    }

    /// The ledger check and sample write share the transaction with all deletion writers.
    pub fn complete_fingerprint_attempt_for_source(
        &self,
        attempt: &StoredFingerprintAttempt,
        sample: &StoredFingerprintSample,
        expected_path: &str,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| {
            db.complete_fingerprint_attempt_for_source(attempt, sample, expected_path)
        })
    }

    pub fn list_fingerprint_attempts_for_job(
        &self,
        job_id: &str,
    ) -> Result<Vec<StoredFingerprintAttempt>, StoreError> {
        self.with_library_read(|db| db.list_fingerprint_attempts_for_job(job_id))
    }

    pub fn get_fingerprint_attempt(
        &self,
        attempt_id: &str,
    ) -> Result<Option<StoredFingerprintAttempt>, StoreError> {
        self.with_library_read(|db| db.get_fingerprint_attempt(attempt_id))
    }

    pub fn recover_interrupted_fingerprint_attempts(
        &self,
        job_id: &str,
    ) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.recover_interrupted_fingerprint_attempts(job_id))
    }
}

fn fingerprint_source_active(
    conn: &rusqlite::Connection,
    job_id: &str,
    file_id: &str,
) -> Result<bool, StoreError> {
    conn.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM probe_jobs WHERE id=?1 AND status NOT IN ('queued','running'))
            AND NOT EXISTS(SELECT 1 FROM probe_job_units WHERE job_id=?1 AND file_id=?2
                           AND status IN ('cancelled','failed'))",
        params![job_id, file_id],
        |row| row.get(0),
    ).map_err(Into::into)
}

#[path = "fingerprint_attempts/connection.rs"]
mod connection;
