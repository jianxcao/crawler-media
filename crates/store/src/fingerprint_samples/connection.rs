//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn find_covering_fingerprint_samples(
        &self,
        query: &FingerprintSampleQuery,
    ) -> Result<Vec<StoredFingerprintSample>, StoreError> {
        let mut sql = String::from(
            "SELECT sample_id, file_id, source_version, capture_profile_key, kind,
                    window_start_ms, window_end_ms, pcm_duration_ms, fingerprint_json,
                    captured_job_id, captured_at_ms, metrics_json
             FROM fingerprint_samples
             WHERE file_id = ?1
               AND source_version = ?2
               AND capture_profile_key = ?3
               AND kind = ?4
               AND window_start_ms <= ?5
               AND window_end_ms >= ?6",
        );

        if query.captured_job_id.is_some() {
            sql.push_str(" AND captured_job_id = ?7");
        }

        sql.push_str(" ORDER BY window_start_ms ASC, rowid DESC");

        let mut stmt = self.library.prepare(&sql)?;

        let mapper = |row: &rusqlite::Row<'_>| {
            let fp_str: String = row.get(8)?;
            let fingerprint: Vec<u32> = serde_json::from_str(&fp_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(StoredFingerprintSample {
                sample_id: row.get(0)?,
                ledger_id: row.get(1)?,
                source_version: row.get(2)?,
                capture_profile_key: row.get(3)?,
                kind: row.get(4)?,
                window_start_ms: row.get(5)?,
                window_end_ms: row.get(6)?,
                pcm_duration_ms: row.get(7)?,
                fingerprint,
                captured_job_id: row.get(9)?,
                captured_at_ms: row.get(10)?,
                metrics_json: row.get(11)?,
            })
        };

        let rows = if let Some(job_id) = &query.captured_job_id {
            stmt.query_map(
                params![
                    query.ledger_id,
                    query.source_version,
                    query.capture_profile_key,
                    query.kind,
                    query.window_start_ms,
                    query.window_end_ms,
                    job_id,
                ],
                mapper,
            )?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(
                params![
                    query.ledger_id,
                    query.source_version,
                    query.capture_profile_key,
                    query.kind,
                    query.window_start_ms,
                    query.window_end_ms,
                ],
                mapper,
            )?
            .collect::<Result<Vec<_>, _>>()?
        };

        Ok(rows)
    }
}
