use rusqlite::params;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFingerprintSample {
    pub sample_id: String,
    pub ledger_id: String,
    pub source_version: String,
    pub capture_profile_key: String,
    pub kind: String,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub pcm_duration_ms: Option<i64>,
    pub fingerprint: Vec<u32>,
    pub captured_job_id: String,
    pub captured_at_ms: i64,
    pub metrics_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FingerprintSampleQuery {
    pub ledger_id: String,
    pub source_version: String,
    pub capture_profile_key: String,
    pub kind: String,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub captured_job_id: Option<String>,
}

impl Store {
    pub fn find_covering_fingerprint_samples(
        &self,
        query: &FingerprintSampleQuery,
    ) -> Result<Vec<StoredFingerprintSample>, StoreError> {
        self.with_library_read(|db| db.find_covering_fingerprint_samples(query))
    }
}

// Also used inside the transaction that removes a ledger and its other caches.
pub(crate) fn delete_samples_for_ledger(
    tx: &rusqlite::Connection,
    ledger_id: &str,
) -> Result<(), StoreError> {
    // First delete models that reference samples belonging to this ledger
    tx.execute(
        "DELETE FROM fingerprint_season_models
             WHERE model_id IN (
                 SELECT model_id FROM fingerprint_model_members WHERE file_id = ?1
             )",
        params![ledger_id],
    )?;
    tx.execute(
        "DELETE FROM fingerprint_samples WHERE file_id = ?1",
        params![ledger_id],
    )?;
    Ok(())
}

#[path = "fingerprint_samples/connection.rs"]
mod connection;
