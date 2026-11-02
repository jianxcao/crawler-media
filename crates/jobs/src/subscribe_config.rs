//! Idempotent publication of one Subscribe configuration and its receipt.
use crate::{JobError, Queue};
use rusqlite::OptionalExtension;

impl Queue {
    /// A receipt is committed atomically with changes. Replaying an applied
    /// revision acknowledges it without overwriting subsequent manual Jobs edits.
    pub fn apply_subscribe_config(
        &self,
        subscribe_id: domain::SubscribeId,
        generation: &str,
        revision: i64,
        work: impl FnOnce(&Self) -> Result<(), JobError>,
    ) -> Result<bool, JobError> {
        self.with_connection(|connection| {
            let tx = rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)?;
            let applied = tx.query_row(
                "SELECT revision FROM subscribe_config_receipts WHERE subscribe_id=?1 AND generation=?2",
                rusqlite::params![subscribe_id.to_string(), generation],
                |row| row.get::<_, i64>(0)).optional()?;
            if applied.is_some_and(|applied| applied >= revision) { return Ok(false); }
            work(self)?;
            tx.execute(
                "INSERT INTO subscribe_config_receipts(subscribe_id,generation,revision) VALUES(?1,?2,?3)
                 ON CONFLICT(subscribe_id,generation) DO UPDATE SET revision=excluded.revision",
                rusqlite::params![subscribe_id.to_string(),generation,revision])?;
            tx.commit()?;
            Ok(true)
        })
    }
}
