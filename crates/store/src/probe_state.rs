pub mod retry;

use rusqlite::{params, OptionalExtension, Transaction};
use super::{Store, StoreError};

pub use retry::retry_delay_ms;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStage {
    Metadata,
    Intro,
    Outro,
}

impl ProbeStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Intro => "intro",
            Self::Outro => "outro",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "metadata" => Some(Self::Metadata),
            "intro" => Some(Self::Intro),
            "outro" => Some(Self::Outro),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStageStatus {
    Pending,
    Queued,
    Running,
    Succeeded,
    Failed,
    NotApplicable,
    Cancelled,
}

impl ProbeStageStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::NotApplicable => "not_applicable",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "not_applicable" => Some(Self::NotApplicable),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeStageKey {
    pub ledger_id: String,
    pub context_key: String,
    pub stage: ProbeStage,
}

#[derive(Clone, Debug)]
pub struct ProbeStageState {
    pub key: ProbeStageKey,
    pub status: ProbeStageStatus,
    pub failure_count: u32,
    pub next_retry_at_ms: Option<i64>,
    pub error_kind: Option<String>,
    pub error: Option<String>,
    pub active_job_id: Option<String>,
}

#[derive(Clone, Debug)]
pub enum ProbeStageCompletion {
    Succeeded,
    Failed { error_kind: String, error: String },
    NotApplicable,
    Cancelled { reason: String },
}

impl Store {
    pub fn get_probe_stage(&self, key: &ProbeStageKey) -> Result<Option<ProbeStageState>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT status, failure_count, next_retry_at_ms, error_kind, error, active_job_id
             FROM probe_stage_state
             WHERE ledger_id = ?1 AND context_key = ?2 AND stage = ?3"
        )?;
        let row = stmt.query_row(
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
            }
        ).optional()?;
        Ok(row)
    }

    pub fn claim_probe_stage(
        &self,
        key: &ProbeStageKey,
        job_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let tx = self.library.unchecked_transaction()?;
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
        let tx = self.library.unchecked_transaction()?;
        let (status, err_kind, err_msg, next_retry, inc_fail) = match completion {
            ProbeStageCompletion::Succeeded => (ProbeStageStatus::Succeeded, None, None, None, false),
            ProbeStageCompletion::NotApplicable => (ProbeStageStatus::NotApplicable, None, None, None, false),
            ProbeStageCompletion::Cancelled { reason } => (ProbeStageStatus::Cancelled, Some("cancelled".to_string()), Some(reason.clone()), None, false),
            ProbeStageCompletion::Failed { error_kind, error } => {
                let current_fail: u32 = tx.query_row(
                    "SELECT failure_count FROM probe_stage_state WHERE ledger_id = ?1 AND context_key = ?2 AND stage = ?3",
                    params![key.ledger_id, key.context_key, key.stage.as_str()],
                    |r| r.get(0),
                ).optional()?.unwrap_or(0);
                let new_fail = current_fail + 1;
                let delay = retry_delay_ms(new_fail);
                let next_retry = delay.map(|d| now_ms + d);
                (ProbeStageStatus::Failed, Some(error_kind.clone()), Some(error.clone()), next_retry, true)
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
                     updated_at_ms = ?8
                 WHERE ledger_id = ?1 AND context_key = ?2 AND stage = ?3 AND active_job_id = ?9",
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
                     updated_at_ms = ?8
                 WHERE ledger_id = ?1 AND context_key = ?2 AND stage = ?3 AND active_job_id = ?9",
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

    pub fn list_due_probe_stages(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<ProbeStageState>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT ledger_id, context_key, stage, status, failure_count, next_retry_at_ms, error_kind, error, active_job_id
             FROM probe_stage_state
             WHERE status = 'failed' AND next_retry_at_ms IS NOT NULL AND next_retry_at_ms <= ?1 AND active_job_id IS NULL
             ORDER BY next_retry_at_ms ASC
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![now_ms, limit as i64], |row| {
            let ledger_id: String = row.get(0)?;
            let context_key: String = row.get(1)?;
            let stage_str: String = row.get(2)?;
            let stage = ProbeStage::from_str(&stage_str).unwrap_or(ProbeStage::Metadata);
            let status_str: String = row.get(3)?;
            let status = ProbeStageStatus::from_str(&status_str).unwrap_or(ProbeStageStatus::Failed);
            let failure_count: u32 = row.get(4)?;
            let next_retry_at_ms: Option<i64> = row.get(5)?;
            let error_kind: Option<String> = row.get(6)?;
            let error: Option<String> = row.get(7)?;
            let active_job_id: Option<String> = row.get(8)?;
            Ok(ProbeStageState {
                key: ProbeStageKey {
                    ledger_id,
                    context_key,
                    stage,
                },
                status,
                failure_count,
                next_retry_at_ms,
                error_kind,
                error,
                active_job_id,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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

    pub fn reset_probe_stage_failure(
        &self,
        key: &ProbeStageKey,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let count = self.library.execute(
            "UPDATE probe_stage_state
             SET status = 'pending', failure_count = 0, next_retry_at_ms = NULL, error = NULL, error_kind = NULL, updated_at_ms = ?4
             WHERE ledger_id = ?1 AND context_key = ?2 AND stage = ?3 AND active_job_id IS NULL",
            params![key.ledger_id, key.context_key, key.stage.as_str(), now_ms],
        )?;
        Ok(count > 0)
    }
}

pub(crate) fn claim_probe_stage_tx(
    tx: &Transaction<'_>,
    key: &ProbeStageKey,
    job_id: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let existing: Option<(String, Option<String>, Option<i64>)> = tx.query_row(
        "SELECT status, active_job_id, next_retry_at_ms FROM probe_stage_state
         WHERE ledger_id = ?1 AND context_key = ?2 AND stage = ?3",
        params![key.ledger_id, key.context_key, key.stage.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?;

    match existing {
        Some((status, active_job, next_retry)) => {
            if active_job.is_some() || status == "running" {
                return Ok(false);
            }
            if status == "failed" {
                if let Some(due) = next_retry {
                    if due > now_ms {
                        return Ok(false);
                    }
                }
            }
            let updated = tx.execute(
                "UPDATE probe_stage_state
                 SET status = 'running', active_job_id = ?4, updated_at_ms = ?5
                 WHERE ledger_id = ?1 AND context_key = ?2 AND stage = ?3 AND active_job_id IS NULL",
                params![key.ledger_id, key.context_key, key.stage.as_str(), job_id, now_ms],
            )?;
            Ok(updated > 0)
        }
        None => {
            let inserted = tx.execute(
                "INSERT INTO probe_stage_state (
                     ledger_id, context_key, stage, status, failure_count, active_job_id, updated_at_ms
                 ) VALUES (?1, ?2, ?3, 'running', 0, ?4, ?5)",
                params![key.ledger_id, key.context_key, key.stage.as_str(), job_id, now_ms],
            )?;
            Ok(inserted > 0)
        }
    }
}

pub(crate) fn delete_probe_stages_for_ledger(
    tx: &Transaction<'_>,
    ledger_id: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM probe_stage_state WHERE ledger_id = ?1",
        params![ledger_id],
    )?;
    Ok(())
}
