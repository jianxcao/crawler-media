//! Marker publication source checks.
//!
//! Disk observation never happens inside the store lock or a write transaction:
//! callers take an owned [`SourceSnapshot`] under a short read, observe it with
//! [`observe_sources`] outside every guard, and hand the resulting
//! [`SourceObservation`]s to the checked publication. The transaction only
//! compares ledger rows, versions, membership and job state against them.
//! Filesystem and DB are not atomic: a file changed after the last observation
//! is caught by the source-version barrier and the next reconcile, not here.

use super::{MarkerResultReplacement, Store, StoreError};
use domain::{FilePresence, FileVersion, LedgerId};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Owned ledger snapshot of a source file, taken under a short store read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub file_id: LedgerId,
    pub version: FileVersion,
    pub path: PathBuf,
}

/// Result of observing a [`SourceSnapshot`] on disk outside every store guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceObservation {
    pub file_id: LedgerId,
    pub expected: FileVersion,
    pub presence: FilePresence,
}

/// Observe sources on disk. Must be called without holding the store lock.
pub fn observe_sources(snapshots: &[SourceSnapshot]) -> Vec<SourceObservation> {
    observe_sources_with(snapshots, observe_path)
}

/// [`observe_sources`] with an injectable observer (tests gate slow IO with it).
pub fn observe_sources_with(
    snapshots: &[SourceSnapshot],
    mut observe: impl FnMut(&Path) -> FilePresence,
) -> Vec<SourceObservation> {
    snapshots
        .iter()
        .map(|snapshot| SourceObservation {
            file_id: snapshot.file_id,
            expected: snapshot.version.clone(),
            presence: observe(&snapshot.path),
        })
        .collect()
}

fn observe_path(path: &Path) -> FilePresence {
    match path.try_exists() {
        Ok(true) => FilePresence::Present,
        Ok(false) => FilePresence::Missing,
        Err(error) => {
            tracing::error!(%error, path = %path.display(), "观察标记发布源文件失败");
            FilePresence::Unknown
        }
    }
}

impl Store {
    /// Owned ledger snapshots for `file_ids`; ids without a row are omitted.
    pub fn source_snapshots(
        &self,
        file_ids: &[LedgerId],
    ) -> Result<Vec<SourceSnapshot>, StoreError> {
        let mut statement = self.library.prepare_cached(
            "SELECT file_revision, source_version, display_path FROM library_files WHERE file_id = ?1",
        )?;
        let mut snapshots = Vec::with_capacity(file_ids.len());
        for file_id in file_ids {
            let row = statement
                .query_row([file_id.to_string()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .optional()?;
            if let Some((revision, source_version, path)) = row {
                snapshots.push(SourceSnapshot {
                    file_id: *file_id,
                    version: FileVersion {
                        revision,
                        source_version,
                    },
                    path: PathBuf::from(path),
                });
            }
        }
        Ok(snapshots)
    }

    pub fn complete_marker_refresh_checked(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
        members: &[SourceObservation],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id), Some(members))
    }

    pub fn replace_marker_results_batch_checked(
        &self,
        replacements: &[MarkerResultReplacement],
        members: &[SourceObservation],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None, Some(members))
    }
}

/// Compare one published file against its out-of-lock observation.
pub(super) fn check_observed(
    tx: &Transaction<'_>,
    file_id: &str,
    members: &[SourceObservation],
) -> Result<(), StoreError> {
    let current: Option<(i64, String)> = tx
        .prepare_cached(
            "SELECT file_revision, source_version FROM library_files WHERE file_id = ?1",
        )?
        .query_row([file_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((revision, source_version)) = current else {
        return Err(StoreError::Missing(format!("file_deleted: {file_id}")));
    };
    let Some(observation) = members.iter().find(|m| m.file_id.to_string() == file_id) else {
        return Err(StoreError::Missing(format!("source_unobserved: {file_id}")));
    };
    if observation.expected.revision != revision
        || observation.expected.source_version != source_version
    {
        return Err(StoreError::Missing(format!("source_changed: {file_id}")));
    }
    match observation.presence {
        FilePresence::Present => Ok(()),
        FilePresence::Missing => Err(StoreError::Missing(format!("file_deleted: {file_id}"))),
        FilePresence::Unknown => {
            tracing::error!(file_id, "标记发布源文件状态未知，保留现有标记");
            Err(StoreError::Missing(format!("source_unknown: {file_id}")))
        }
    }
}

pub(super) fn validate_members(
    tx: &Transaction<'_>,
    replacements: &[MarkerResultReplacement],
    members: &[SourceObservation],
) -> Result<(), StoreError> {
    for replacement in replacements {
        let expected: BTreeSet<_> = replacement
            .chapter_updates
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        let mut statement = tx.prepare(
            "SELECT DISTINCT f.file_id
             FROM library_files f
             LEFT JOIN library_file_units u ON u.file_id = f.file_id
             WHERE f.media_id = ?1 AND COALESCE(NULLIF(u.season, -1), 1) = ?2",
        )?;
        let actual = statement
            .query_map(
                params![replacement.media_id.to_string(), replacement.season],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<BTreeSet<_>, _>>()?;
        if actual != expected {
            return Err(StoreError::Missing(
                "file_deleted: season membership changed before publication".into(),
            ));
        }
        for id in &actual {
            check_observed(tx, id, members)?;
        }
    }
    Ok(())
}

pub(super) fn validate_job_members(
    tx: &Transaction<'_>,
    job_id: &str,
    replacements: &[MarkerResultReplacement],
    members: &[SourceObservation],
) -> Result<(), StoreError> {
    // Chapter updates are a subset of the files compared. Every file the job leased
    // must still match the observation pinned before comparison, whether or not it
    // produced a chapter update.
    let mut leased = tx.prepare("SELECT file_id FROM probe_job_units WHERE job_id = ?1")?;
    let leased: Vec<String> = leased
        .query_map([job_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for id in &leased {
        check_observed(tx, id, members)?;
    }
    // A partial refresh can use cached samples outside its write lease. They
    // remain dependencies of the result and must be checked in this transaction.
    for observation in members {
        check_observed(tx, &observation.file_id.to_string(), members)?;
    }
    let published: BTreeSet<_> = replacements
        .iter()
        .flat_map(|replacement| replacement.chapter_updates.iter().map(|(id, _)| id.clone()))
        .collect();
    let mut statement = tx.prepare("SELECT file_id FROM probe_job_units WHERE job_id = ?1")?;
    let original: BTreeSet<String> = statement
        .query_map([job_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if published != original {
        return Err(StoreError::Missing(
            "file_deleted: refresh membership changed before publication".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_published_rows(
    tx: &Transaction<'_>,
    replacements: &[MarkerResultReplacement],
    members: &[SourceObservation],
) -> Result<(), StoreError> {
    for replacement in replacements {
        for (id, _) in &replacement.chapter_updates {
            check_observed(tx, id, members)?;
        }
    }
    Ok(())
}
