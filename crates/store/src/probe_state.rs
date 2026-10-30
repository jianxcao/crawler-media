pub mod retry;

use super::{Store, StoreError};
use rusqlite::{OptionalExtension, params};

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

    #[allow(clippy::should_implement_trait)]
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

    #[allow(clippy::should_implement_trait)]
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
    pub fn get_probe_stage(
        &self,
        key: &ProbeStageKey,
    ) -> Result<Option<ProbeStageState>, StoreError> {
        self.with_library_read(|db| db.get_probe_stage(key))
    }

    pub fn claim_probe_stage(
        &self,
        key: &ProbeStageKey,
        job_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.claim_probe_stage(key, job_id, now_ms))
    }

    pub fn finish_probe_stage(
        &self,
        key: &ProbeStageKey,
        job_id: &str,
        completion: &ProbeStageCompletion,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.finish_probe_stage(key, job_id, completion, now_ms))
    }

    /// Claim up to `limit` due failed stages in one transaction, in
    /// `(deadline, file_id, context, stage)` order, leasing each until
    /// `now_ms + lease_ms` so the next ticks skip them while the dispatcher
    /// evaluates and queues them. A finished attempt clears the lease; an
    /// abandoned one simply becomes due again when it expires (restart-safe).
    pub fn claim_due_probe_stages(
        &self,
        now_ms: i64,
        lease_ms: i64,
        limit: usize,
    ) -> Result<Vec<ProbeStageState>, StoreError> {
        self.with_library_write(|db| db.claim_due_probe_stages(now_ms, lease_ms, limit))
    }

    /// Retire one stage row (superseded context, or no longer needed). Deleting
    /// it makes a late worker's job+context CAS miss, and a restart cannot revive it.
    pub fn retire_probe_stage(&self, key: &ProbeStageKey) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.retire_probe_stage(key))
    }

    /// Adopt `context_key` as the file's current probe context: every other
    /// context's stage state is retired in the same short transaction.
    pub fn adopt_probe_context(
        &self,
        ledger_id: &str,
        context_key: &str,
    ) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.adopt_probe_context(ledger_id, context_key))
    }

    pub fn season_comparison_digest(
        &self,
        media_id: &str,
        season: u32,
    ) -> Result<Option<String>, StoreError> {
        self.with_library_read(|db| db.season_comparison_digest(media_id, season))
    }

    pub fn save_season_comparison_digest(
        &self,
        media_id: &str,
        season: u32,
        digest: &str,
        published_at_ms: i64,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| {
            db.save_season_comparison_digest(media_id, season, digest, published_at_ms)
        })
    }
}
pub(crate) fn claim_probe_stage_tx(
    tx: &rusqlite::Connection,
    key: &ProbeStageKey,
    job_id: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let existing: Option<(String, Option<String>, Option<i64>)> = tx
        .query_row(
            "SELECT status, active_job_id, next_retry_at_ms FROM probe_stage_state
         WHERE file_id = ?1 AND context_key = ?2 AND stage = ?3",
            params![key.ledger_id, key.context_key, key.stage.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match existing {
        Some((status, active_job, next_retry)) => {
            if active_job.is_some() || status == "running" {
                return Ok(false);
            }
            if status == "failed"
                && let Some(due) = next_retry
                && due > now_ms
            {
                return Ok(false);
            }
            let updated = tx.execute(
                "UPDATE probe_stage_state
                 SET status = 'running', active_job_id = ?4, updated_at_ms = ?5
                 WHERE file_id = ?1 AND context_key = ?2 AND stage = ?3 AND active_job_id IS NULL",
                params![
                    key.ledger_id,
                    key.context_key,
                    key.stage.as_str(),
                    job_id,
                    now_ms
                ],
            )?;
            Ok(updated > 0)
        }
        None => {
            let inserted = tx.execute(
                "INSERT INTO probe_stage_state (
                     file_id, context_key, stage, status, failure_count, active_job_id, updated_at_ms
                 ) VALUES (?1, ?2, ?3, 'running', 0, ?4, ?5)",
                params![key.ledger_id, key.context_key, key.stage.as_str(), job_id, now_ms],
            )?;
            Ok(inserted > 0)
        }
    }
}

pub(crate) fn delete_probe_stages_for_ledger(
    tx: &rusqlite::Connection,
    ledger_id: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM probe_stage_state WHERE file_id = ?1",
        params![ledger_id],
    )?;
    Ok(())
}

fn stage_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProbeStageState> {
    let stage_str: String = row.get(2)?;
    let status_str: String = row.get(3)?;
    Ok(ProbeStageState {
        key: ProbeStageKey {
            ledger_id: row.get(0)?,
            context_key: row.get(1)?,
            stage: ProbeStage::from_str(&stage_str).unwrap_or(ProbeStage::Metadata),
        },
        status: ProbeStageStatus::from_str(&status_str).unwrap_or(ProbeStageStatus::Failed),
        failure_count: row.get(4)?,
        next_retry_at_ms: row.get(5)?,
        error_kind: row.get(6)?,
        error: row.get(7)?,
        active_job_id: row.get(8)?,
    })
}

#[path = "probe_state/connection.rs"]
mod connection;
