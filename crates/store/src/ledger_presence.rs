//! Presence types and transactional updates for safe verification and pruning.
//!
//! Presence is a property of a `library_files` row: `present` = usable, `missing`,
//! `unknown`. A permission/IO error is never recorded as `missing`.

use domain::{FileCoverage, FileVersion, LedgerId};
use std::collections::HashSet;
mod lifecycle;
mod read;
mod watcher_read;
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

pub use domain::FilePresence;

/// One observed file plus the presence verdict the scan derived for it.
///
/// The file is the frozen read shape ([`FileCoverage`]); identity is read from
/// `file.file_id`/`file.path`/`file.media_id`, so a caller cannot hand over a
/// mismatched id/path pair produced by a different query.
#[derive(Clone, Debug)]
pub struct LedgerPresenceObservation {
    pub file: FileCoverage,
    /// Canonical identity captured with the observation, before the store lock.
    pub path_key: String,
    pub missing_at: Option<i64>,
    pub presence: FilePresence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerPresenceMode {
    Verify,
    Prune,
    /// A full rescan proved the source gone: remove it now, without the
    /// missing-then-prune two-step (2026-10-29 E2E "missing-rows").
    Purge,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LedgerPresenceResult {
    pub missing: usize,
    pub restored: usize,
    pub deleted: usize,
    pub skipped: usize,
    pub applied: Vec<AppliedLedgerPresence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerPresenceOutcome {
    Missing,
    Restored,
    Deleted,
    Published,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedLedgerPresence {
    pub file_id: LedgerId,
    pub version: FileVersion,
    pub outcome: LedgerPresenceOutcome,
}

impl crate::Store {
    /// Canonical identity of a path that may no longer exist. No filesystem IO.
    pub fn recorded_path_key(&self, path: &str) -> String {
        crate::path_key::canonical_recorded_key(&self.library, path)
    }

    /// Apply presence observations and probe lifecycle within one Immediate transaction.
    /// `now` is Unix seconds for missing_at; probe lifecycle timestamps use milliseconds.
    ///
    /// For each observation, checks whether the current database row still matches
    /// the observed identity (file id/version, Library/root, path, Media, publication
    /// and missing_at). If it changed,
    /// the row is skipped.
    ///
    /// In Verify mode:
    /// - Missing: sets `missing_at = now` if not already set.
    /// - Present: clears `missing_at` if set.
    /// - Unknown: no change.
    ///
    /// In Prune mode:
    /// - Missing: if it was already marked missing (`missing_at` is Some), removes
    ///   the file row and its file-derived rows.
    /// - Present: clears `missing_at` if set.
    /// - Unknown: no change.
    ///
    /// In Purge mode: Missing removes the row whether or not it was marked
    /// missing; Present clears `missing_at`.
    ///
    /// A `pending` file observed as present is the crash-recovery case: its bytes
    /// completed Transfer, so it becomes published content.
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
        let mut batch = crate::library_write::presence::PresenceBatch::default();
        let mut seen = HashSet::new();
        for obs in observations {
            if !seen.insert(obs.file.file.file_id) {
                result.skipped += 1;
                continue;
            }
            apply_observation(&tx, obs, mode, now, &mut result, &mut batch)?;
        }
        batch.settle(&tx)?;
        if result.deleted > 0 {
            let candidates: HashSet<domain::MediaId> = observations
                .iter()
                .map(|obs| obs.file.file.media_id)
                .collect();
            let subscribed = self.subscribed_media(&candidates)?;
            crate::library_write::orphan_media::purge_orphan_media_in_tx(
                &tx,
                &candidates,
                &subscribed,
            )?;
        }

        tx.commit()?;
        Ok(result)
    }
}

/// Validate the complete observed identity before changing facts or probe lifecycle.
fn apply_observation(
    tx: &Connection,
    obs: &LedgerPresenceObservation,
    mode: LedgerPresenceMode,
    now: i64,
    result: &mut LedgerPresenceResult,
    batch: &mut crate::library_write::presence::PresenceBatch,
) -> Result<(), StoreError> {
    let file = &obs.file.file;
    let file_id = file.file_id.to_string();
    let publication: Option<String> = tx
        .query_row(
            "SELECT publication_state FROM library_files
         WHERE file_id=?1 AND library_id=?2 AND media_id=?3 AND path_key=?4
           AND root_id=?5 AND file_revision=?6 AND source_version=?7 AND missing_at IS ?8
           AND publication_state=?9",
            params![
                file_id,
                file.library_id.to_string(),
                file.media_id.to_string(),
                obs.path_key,
                file.root_id,
                file.version.revision,
                file.version.source_version,
                obs.missing_at,
                file.publication.as_str()
            ],
            |r| r.get(0),
        )
        .optional()?;
    let Some(publication) = publication else {
        result.skipped += 1;
        return Ok(());
    };
    crate::library_write::ensure_library_writable(tx, &file.library_id.to_string())?;
    crate::library_operations::integrity_guard::ensure_integrity_writable(tx, &[file.library_id])?;
    crate::library_write::ensure_media_writable(tx, file.media_id)?;
    let outcome = if publication == "pending" {
        (obs.presence == FilePresence::Present).then_some(LedgerPresenceOutcome::Published)
    } else {
        match (mode, obs.presence, obs.missing_at) {
            (LedgerPresenceMode::Verify, FilePresence::Missing, None) => {
                Some(LedgerPresenceOutcome::Missing)
            }
            (LedgerPresenceMode::Prune, FilePresence::Missing, Some(_))
            | (LedgerPresenceMode::Purge, FilePresence::Missing, _) => {
                Some(LedgerPresenceOutcome::Deleted)
            }
            (_, FilePresence::Present, Some(_)) => Some(LedgerPresenceOutcome::Restored),
            _ => None,
        }
    };
    let Some(outcome) = outcome else {
        result.skipped += 1;
        return Ok(());
    };
    let now_ms = now.checked_mul(1_000).ok_or_else(|| {
        StoreError::Invalid("presence timestamp exceeds milliseconds range".into())
    })?;
    batch.capture(tx, file.library_id, file.media_id, &file_id)?;
    if outcome == LedgerPresenceOutcome::Published {
        tx.execute(
            "UPDATE library_files SET publication_state='published', presence_state='present',
                    missing_at=NULL WHERE file_id=?1",
            [&file_id],
        )?;
        tracing::info!(file_id, "Recovered completed Transfer publication");
    }

    match outcome {
        LedgerPresenceOutcome::Missing => {
            tx.execute(
                "UPDATE library_files SET missing_at=?1, presence_state='missing' WHERE file_id=?2",
                rusqlite::params![now, file_id],
            )?;
            lifecycle::source_missing(tx, &file_id, now_ms)?;
            result.missing += 1;
        }
        LedgerPresenceOutcome::Deleted => {
            crate::ledger_cleanup::delete_file_in_tx(tx, &file_id)?;
            result.deleted += 1;
        }
        LedgerPresenceOutcome::Restored => {
            tx.execute("UPDATE library_files SET missing_at=NULL, presence_state='present' WHERE file_id=?1", [&file_id])?;
            lifecycle::source_restored(tx, &file_id, now_ms)?;
            result.restored += 1;
        }
        LedgerPresenceOutcome::Published => {}
    }
    result.applied.push(AppliedLedgerPresence {
        file_id: obs.file.file.file_id,
        version: obs.file.file.version.clone(),
        outcome,
    });
    tracing::info!(file_id, ?outcome, "Applied Library file presence");

    Ok(())
}
