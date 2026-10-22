//! Presence types and transactional updates for safe verification and pruning.
//!
//! Presence is a property of a `library_files` row: `present` = usable, `missing`,
//! `unknown`. A permission/IO error is never recorded as `missing`.

use domain::{FileCoverage, LibraryId, MediaId};
use rusqlite::Connection;

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

/// The Library and Media a presence write affected, so the summaries can follow.
struct Affected {
    library_id: LibraryId,
    media_id: MediaId,
}

impl crate::Store {
    /// Apply presence observations within a single Immediate transaction in library.db.
    ///
    /// For each observation, checks whether the current database row still matches
    /// the observed identity (file id, path, media_id, and missing_at). If it changed,
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
        let mut affected: Vec<Affected> = Vec::new();
        for obs in observations {
            if let Some(scope) = apply_observation(&tx, obs, mode, now, &mut result)? {
                affected.push(scope);
            }
        }
        for scope in &affected {
            crate::library_write::reduce_library_media(&tx, scope.library_id, scope.media_id)?;
        }

        tx.commit()?;
        Ok(result)
    }
}

/// Apply one observation; returns the scope whose summaries must be reduced.
fn apply_observation(
    tx: &Connection,
    obs: &LedgerPresenceObservation,
    mode: LedgerPresenceMode,
    now: i64,
    result: &mut LedgerPresenceResult,
) -> Result<Option<Affected>, StoreError> {
    let file_id = obs.file.file.file_id.to_string();
    let current: Option<(String, String, String, Option<i64>, String)> = tx
        .query_row(
            "SELECT library_id, media_id, path_key, missing_at, publication_state
             FROM library_files WHERE file_id = ?1",
            [&file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .rusqlite_optional()?;

    let Some((library_id, media_id, path, missing_at, publication)) = current else {
        result.skipped += 1;
        return Ok(None);
    };
    // 与既有行为一致：库里存的是 `path_key`，观察值来自 `display_path`；
    // 两者在规范化/大小写不同的场景下本来就可能不等（已记录的容忍项，本轮不统一）。
    let observed_path = obs.file.file.path.to_string_lossy();
    if path != observed_path.as_ref()
        || media_id != obs.file.file.media_id.to_string()
        || missing_at != obs.missing_at
    {
        result.skipped += 1;
        return Ok(None);
    }
    let scope = Affected {
        library_id: library_id.parse::<LibraryId>()?,
        media_id: media_id.parse::<MediaId>()?,
    };

    if publication == "pending" {
        if obs.presence != FilePresence::Present {
            result.skipped += 1;
            return Ok(None);
        }
        tx.execute(
            "UPDATE library_files
             SET publication_state = 'published', presence_state = 'present', missing_at = NULL
             WHERE file_id = ?1",
            [&file_id],
        )?;
        tracing::info!(file_id = %obs.file.file.file_id, path = %obs.file.file.path.display(),
            "Recovered completed Transfer publication");
        return Ok(Some(scope));
    }

    match (mode, obs.presence) {
        (LedgerPresenceMode::Verify, FilePresence::Missing) => {
            if missing_at.is_none() {
                tx.execute(
                    "UPDATE library_files SET missing_at = ?1, presence_state = 'missing'
                     WHERE file_id = ?2",
                    rusqlite::params![now, file_id],
                )?;
                result.missing += 1;
                return Ok(Some(scope));
            }
            result.skipped += 1;
        }
        (LedgerPresenceMode::Prune, FilePresence::Missing) => {
            if missing_at.is_some() {
                crate::ledger_cleanup::delete_file_in_tx(tx, &file_id)?;
                result.deleted += 1;
                return Ok(Some(scope));
            }
            result.skipped += 1;
        }
        (_, FilePresence::Present) => {
            if missing_at.is_some() {
                tx.execute(
                    "UPDATE library_files SET missing_at = NULL, presence_state = 'present'
                     WHERE file_id = ?1",
                    [&file_id],
                )?;
                result.restored += 1;
                return Ok(Some(scope));
            }
            result.skipped += 1;
        }
        (_, FilePresence::Unknown) => {
            result.skipped += 1;
        }
    }
    Ok(None)
}

trait RusqliteOptionalExt<T> {
    fn rusqlite_optional(self) -> Result<Option<T>, StoreError>;
}

impl<T> RusqliteOptionalExt<T> for Result<T, rusqlite::Error> {
    fn rusqlite_optional(self) -> Result<Option<T>, StoreError> {
        match self {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}
