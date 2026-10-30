use rusqlite::{OptionalExtension, params};

use super::super::{ProbeJob, Store, StoreError};
use super::{now_ms, probe_job_terminal_status};

#[derive(Clone, Copy, Debug, Default)]
struct ProbeCounts {
    completed: usize,
    succeeded: usize,
    failed: usize,
    partial: usize,
}

impl ProbeCounts {
    fn for_status(status: &str) -> Self {
        let mut counts = Self {
            completed: 1,
            ..Self::default()
        };
        match status {
            "succeeded" => counts.succeeded = 1,
            "failed" => counts.failed = 1,
            "partial" => counts.partial = 1,
            "cancelled" => {}
            _ => counts.completed = 0,
        }
        counts
    }
}

impl Store {}

pub(super) fn reconcile_counts_in_transaction(
    tx: &rusqlite::Connection,
    job_id: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE probe_jobs SET
             completed=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=?1 AND status IN ('succeeded','failed','cancelled','partial')),
             succeeded=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=?1 AND status='succeeded'),
             failed=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=?1 AND status='failed'),
             partial=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=?1 AND status='partial')
         WHERE id=?1",
        [job_id],
    )?;
    Ok(())
}

#[path = "counters/connection.rs"]
mod connection;
