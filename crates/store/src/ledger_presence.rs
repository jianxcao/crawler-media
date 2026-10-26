//! Ledger presence types and transactional updates for safe verification and pruning.

use domain::LedgerRow;
use rusqlite::{OptionalExtension, Transaction};

use crate::StoreError;

mod probe_work;
pub(crate) use probe_work::repair_missing_probe_work;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePresence {
    Present,
    Missing,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct LedgerPresenceObservation {
    pub row: LedgerRow,
    pub missing_at: Option<i64>,
    pub presence: FilePresence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerPresenceMode {
    Verify,
    Prune,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerPresenceResult {
    pub missing: usize,
    pub restored: usize,
    pub deleted: usize,
    pub skipped: usize,
}

impl crate::Store {
    /// Clear a restored source's missing marker and schedule its unfinished probe work atomically.
    pub fn clear_ledger_missing(&self, path: &str) -> Result<bool, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let id: Option<String> = tx
            .query_row(
                "SELECT id FROM ledger WHERE path = ?1 AND missing_at IS NOT NULL",
                [path],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(false);
        };
        tx.execute("UPDATE ledger SET missing_at = NULL WHERE id = ?1", [&id])?;
        probe_work::restore_probe_obligation(&tx, &id, crate::unix_now() * 1000)?;
        tx.commit()?;
        Ok(true)
    }

    /// Whether this ledger source has been confirmed missing by presence reconciliation.
    pub fn ledger_is_missing(&self, ledger_id: &str) -> Result<bool, StoreError> {
        self.library
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ledger WHERE id = ?1 AND missing_at IS NOT NULL)",
                [ledger_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Apply presence observations within a single Immediate transaction in library.db.
    ///
    /// For each observation, checks whether the current database row still matches
    /// the observed identity (id, path, media_id, and missing_at). If it changed,
    /// the row is skipped.
    ///
    /// In Verify mode:
    /// - Missing: sets missing_at = now if not already set.
    /// - Present: clears missing_at if set.
    /// - Unknown: no change.
    ///
    /// In Prune mode:
    /// - Missing: if it was already marked missing (missing_at is Some), deletes
    ///   the ledger row and file-derived rows via `delete_ledger_in_tx`.
    /// - Present: clears missing_at if set.
    /// - Unknown: no change.
    pub fn apply_ledger_presence(
        &self,
        observations: &[LedgerPresenceObservation],
        mode: LedgerPresenceMode,
        now: i64,
    ) -> Result<LedgerPresenceResult, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        let mut result = LedgerPresenceResult::default();

        for obs in observations {
            let current: Option<(String, String, Option<i64>, bool)> = tx
                .query_row(
                    "SELECT path, media_id, missing_at, transfer_pending FROM ledger WHERE id = ?1",
                    [&obs.row.id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;

            let Some((cur_path, cur_media_id, cur_missing_at, pending)) = current else {
                result.skipped += 1;
                continue;
            };

            // Check identity
            if cur_path != obs.row.path
                || cur_media_id != obs.row.media_id.to_string()
                || cur_missing_at != obs.missing_at
            {
                result.skipped += 1;
                continue;
            }
            if pending {
                if obs.presence != FilePresence::Present {
                    result.skipped += 1;
                    continue;
                }
                tx.execute(
                    "UPDATE ledger SET transfer_pending = 0 WHERE id = ?1",
                    [obs.row.id.to_string()],
                )?;
                tracing::info!(ledger_id = %obs.row.id, path = %obs.row.path,
                    "Recovered completed Transfer publication");
            }

            apply_presence_change(&tx, obs, mode, now, &mut result)?;
        }

        tx.commit()?;
        Ok(result)
    }
}

fn apply_presence_change(
    tx: &Transaction<'_>,
    obs: &LedgerPresenceObservation,
    mode: LedgerPresenceMode,
    now: i64,
    result: &mut LedgerPresenceResult,
) -> Result<(), StoreError> {
    let id = obs.row.id.to_string();
    match (mode, obs.presence) {
        (_, FilePresence::Present) => {
            if obs.missing_at.is_some() {
                tx.execute("UPDATE ledger SET missing_at = NULL WHERE id = ?1", [&id])?;
                probe_work::restore_probe_obligation(tx, &id, now * 1000)?;
                result.restored += 1;
            }
        }
        (_, FilePresence::Unknown) => result.skipped += 1,
        (LedgerPresenceMode::Verify, FilePresence::Missing) => {
            if obs.missing_at.is_none() {
                tx.execute(
                    "UPDATE ledger SET missing_at = ?1 WHERE id = ?2",
                    rusqlite::params![now, id],
                )?;
                result.missing += 1;
            }
            probe_work::cancel_missing_probe_work(tx, &id)?;
        }
        (LedgerPresenceMode::Prune, FilePresence::Missing) => {
            if obs.missing_at.is_some() {
                crate::ledger_cleanup::delete_ledger_in_tx(tx, obs.row.id)?;
                result.deleted += 1;
            } else {
                result.skipped += 1;
            }
        }
    }
    Ok(())
}
