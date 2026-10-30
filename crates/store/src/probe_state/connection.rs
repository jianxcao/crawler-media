//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn get_probe_stage(
        &self,
        key: &ProbeStageKey,
    ) -> Result<Option<ProbeStageState>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT status, failure_count, next_retry_at_ms, error_kind, error, active_job_id
             FROM probe_stage_state
             WHERE file_id = ?1 AND context_key = ?2 AND stage = ?3",
        )?;
        let row = stmt
            .query_row(
                params![key.ledger_id, key.context_key, key.stage.as_str()],
                |row| {
                    let status_str: String = row.get(0)?;
                    let status = ProbeStageStatus::from_str(&status_str)
                        .unwrap_or(ProbeStageStatus::Pending);
                    let failure_count: u32 = row.get(1)?;
                    let next_retry_at_ms: Option<i64> = row.get(2)?;
                    let error_kind: Option<String> = row.get(3)?;
                    let error: Option<String> = row.get(4)?;
                    let active_job_id: Option<String> = row.get(5)?;
                    Ok(ProbeStageState {
                        key: key.clone(),
                        status,
                        failure_count,
                        next_retry_at_ms,
                        error_kind,
                        error,
                        active_job_id,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn claim_probe_stage(
        &self,
        key: &ProbeStageKey,
        job_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let claimed = claim_probe_stage_tx(&tx, key, job_id, now_ms)?;
        if claimed {
            tx.commit()?;
        }
        Ok(claimed)
    }

    pub fn finish_probe_stage(
        &self,
        key: &ProbeStageKey,
        job_id: &str,
        completion: &ProbeStageCompletion,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let (status, err_kind, err_msg, next_retry, inc_fail) = match completion {
            ProbeStageCompletion::Succeeded => {
                (ProbeStageStatus::Succeeded, None, None, None, false)
            }
            ProbeStageCompletion::NotApplicable => {
                (ProbeStageStatus::NotApplicable, None, None, None, false)
            }
            ProbeStageCompletion::Cancelled { reason } => (
                ProbeStageStatus::Cancelled,
                Some("cancelled".to_string()),
                Some(reason.clone()),
                None,
                false,
            ),
            ProbeStageCompletion::Failed { error_kind, error } => {
                let current_fail: u32 = tx.query_row(
                    "SELECT failure_count FROM probe_stage_state WHERE file_id = ?1 AND context_key = ?2 AND stage = ?3",
                    params![key.ledger_id, key.context_key, key.stage.as_str()],
                    |r| r.get(0),
                ).optional()?.unwrap_or(0);
                let new_fail = current_fail + 1;
                let delay = retry_delay_ms(new_fail);
                let next_retry = delay.map(|d| now_ms + d);
                (
                    ProbeStageStatus::Failed,
                    Some(error_kind.clone()),
                    Some(error.clone()),
                    next_retry,
                    true,
                )
            }
        };

        let updated = if inc_fail {
            tx.execute(
                "UPDATE probe_stage_state
                 SET status = ?4,
                     failure_count = failure_count + 1,
                     next_retry_at_ms = ?5,
                     error_kind = ?6,
                     error = ?7,
                     active_job_id = NULL,
                     dispatched_until_ms = NULL,
                     updated_at_ms = ?8
                 WHERE file_id = ?1 AND context_key = ?2 AND stage = ?3 AND active_job_id = ?9",
                params![
                    key.ledger_id,
                    key.context_key,
                    key.stage.as_str(),
                    status.as_str(),
                    next_retry,
                    err_kind,
                    err_msg,
                    now_ms,
                    job_id,
                ],
            )?
        } else {
            tx.execute(
                "UPDATE probe_stage_state
                 SET status = ?4,
                     next_retry_at_ms = ?5,
                     error_kind = ?6,
                     error = ?7,
                     active_job_id = NULL,
                     dispatched_until_ms = NULL,
                     updated_at_ms = ?8
                 WHERE file_id = ?1 AND context_key = ?2 AND stage = ?3 AND active_job_id = ?9",
                params![
                    key.ledger_id,
                    key.context_key,
                    key.stage.as_str(),
                    status.as_str(),
                    next_retry,
                    err_kind,
                    err_msg,
                    now_ms,
                    job_id,
                ],
            )?
        };

        if updated > 0 {
            tx.commit()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn claim_due_probe_stages(
        &self,
        now_ms: i64,
        lease_ms: i64,
        limit: usize,
    ) -> Result<Vec<ProbeStageState>, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let rows = {
            let mut stmt = tx.prepare(
                "SELECT file_id, context_key, stage, status, failure_count, next_retry_at_ms, error_kind, error, active_job_id
                 FROM probe_stage_state
                 WHERE status = 'failed' AND next_retry_at_ms IS NOT NULL AND active_job_id IS NULL
                   AND next_retry_at_ms <= ?1
                   AND (dispatched_until_ms IS NULL OR dispatched_until_ms <= ?1)
                 ORDER BY next_retry_at_ms, file_id, context_key, stage
                 LIMIT ?2",
            )?;
            stmt.query_map(params![now_ms, limit as i64], stage_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        {
            let mut lease = tx.prepare_cached(
                "UPDATE probe_stage_state SET dispatched_until_ms = ?4
                 WHERE file_id = ?1 AND context_key = ?2 AND stage = ?3",
            )?;
            for row in &rows {
                lease.execute(params![
                    row.key.ledger_id,
                    row.key.context_key,
                    row.key.stage.as_str(),
                    now_ms.saturating_add(lease_ms)
                ])?;
            }
        }
        tx.commit()?;
        Ok(rows)
    }

    pub fn retire_probe_stage(&self, key: &ProbeStageKey) -> Result<bool, StoreError> {
        Ok(self.library.execute(
            "DELETE FROM probe_stage_state WHERE file_id = ?1 AND context_key = ?2 AND stage = ?3",
            params![key.ledger_id, key.context_key, key.stage.as_str()],
        )? > 0)
    }

    pub fn adopt_probe_context(
        &self,
        ledger_id: &str,
        context_key: &str,
    ) -> Result<usize, StoreError> {
        Ok(self.library.execute(
            "DELETE FROM probe_stage_state WHERE file_id = ?1 AND context_key <> ?2",
            params![ledger_id, context_key],
        )?)
    }

    pub fn season_comparison_digest(
        &self,
        media_id: &str,
        season: u32,
    ) -> Result<Option<String>, StoreError> {
        self.library
            .query_row(
                "SELECT comparison_digest FROM probe_season_comparison_history
                 WHERE media_id = ?1 AND season = ?2",
                params![media_id, season],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn save_season_comparison_digest(
        &self,
        media_id: &str,
        season: u32,
        digest: &str,
        published_at_ms: i64,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "INSERT INTO probe_season_comparison_history (
                 media_id, season, comparison_digest, published_at_ms
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(media_id, season) DO UPDATE SET
               comparison_digest = excluded.comparison_digest,
               published_at_ms = excluded.published_at_ms",
            params![media_id, season, digest, published_at_ms],
        )?;
        Ok(())
    }
}
