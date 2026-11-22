//! Durable file rename / re-parent with sidecar intents and crash recovery.
//!
//! Same-root rename, same-Library cross-root re-parent, and cross-Library
//! re-parent all share one persisted intent: observe sidecars outside the
//! Store lock, register the plan, move bytes with the lock released, then CAS
//! publish path/root/library + derived facts. A crash between any of those
//! steps leaves a resumable `pending_file_renames` row.

use std::path::{Path, PathBuf};

use domain::{FileVersion, LedgerId, LibraryId, MediaId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::publish::ensure_media_writable;
use super::reduce::reduce_library_media;
use super::rename::{Owned, load_owned};
pub use super::rename_disk::RenameSourceEvidence;
use super::rename_record::{RawRename, set_phase};
pub use super::rename_sidecars::{SidecarAction, SidecarSourceIdentity};
use crate::{Store, StoreError};

/// After this many failed recovery attempts an active rename intent becomes
/// terminally `failed` so recovery workers stop retrying it indefinitely.
pub const MAX_FILE_RENAME_ATTEMPTS: i64 = 20;

/// One sidecar that must move with the video (NFO / scrape-registered path).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSidecar {
    pub kind: String,
    pub from: PathBuf,
    pub to: PathBuf,
    /// Durable decision; see [`SidecarAction`].
    pub action: SidecarAction,
    /// Native source identity captured when `Move`/`AlreadyIdentical` was decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_identity: Option<SidecarSourceIdentity>,
    /// Native destination identity verified when `AlreadyIdentical` or placed on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_identity: Option<SidecarSourceIdentity>,
}

/// Persisted rename / re-parent intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRenameRequest {
    pub rename_id: String,
    pub file_id: LedgerId,
    pub expected: FileVersion,
    pub from_path: PathBuf,
    pub to_path: PathBuf,
    pub from_library_id: LibraryId,
    pub from_root_id: String,
    pub to_library_id: LibraryId,
    pub to_root_id: String,
    pub sidecars: Vec<RenameSidecar>,
    pub phase: String,
    pub media_id: MediaId,
    pub source_evidence: RenameSourceEvidence,
    /// Destination identity recorded from the disk receipt, once recorded.
    pub disk_receipt: Option<RenameSourceEvidence>,
    pub from_config_revision: i64,
    pub to_config_revision: i64,
}

/// Keeps Library removal from freezing either Library during rename publication.
/// The reservation holds no mutex or database connection while the caller does IO.
#[must_use]
pub struct RenamePublication<'a> {
    store: &'a Store,
    libraries: Vec<String>,
}

impl Drop for RenamePublication<'_> {
    fn drop(&mut self) {
        let mut publications = self.store.writers.rename_publications.lock();
        for library in &self.libraries {
            if let std::collections::hash_map::Entry::Occupied(mut entry) =
                publications.entry(library.clone())
            {
                *entry.get_mut() -= 1;
                if *entry.get() == 0 {
                    entry.remove();
                }
            }
        }
    }
}

impl Store {
    /// Move bytes and finish their ledger publication. A caller can keep the
    /// returned reservation while writing metadata associated with the new path.
    pub fn apply_file_rename(
        &self,
        request: &FileRenameRequest,
        now: i64,
    ) -> Result<RenamePublication<'_>, StoreError> {
        let publication = self.apply_file_rename_disk(request, now)?;
        self.complete_file_rename(&request.rename_id, now)?;
        Ok(publication)
    }

    fn apply_file_rename_disk(
        &self,
        request: &FileRenameRequest,
        now: i64,
    ) -> Result<RenamePublication<'_>, StoreError> {
        let (receipt, publication) = super::rename_receipt::execute(request, false, || {
            self.admit_rename_publication(request)
        })
        .inspect_err(|error| self.log_rename_refusal(&request.rename_id, now, error))?;
        self.record_rename_disk(&receipt, now)?;
        Ok(publication)
    }

    fn admit_rename_publication(
        &self,
        request: &FileRenameRequest,
    ) -> Result<RenamePublication<'_>, StoreError> {
        let already_moved = request.phase == "disk_applied"
            || (!request.from_path.exists() && request.to_path.exists());
        let from = request.from_library_id.to_string();
        let to = request.to_library_id.to_string();
        let mut libraries = vec![from];
        if to != libraries[0] {
            libraries.push(to);
        }
        self.with_library_write(|db| {
            ensure_rename_libraries_admitted(db.library, request, already_moved)?;
            let mut publications = self.writers.rename_publications.lock();
            for library in &libraries {
                *publications.entry(library.clone()).or_default() += 1;
            }
            Ok(())
        })?;
        Ok(RenamePublication {
            store: self,
            libraries,
        })
    }

    fn log_rename_refusal(&self, rename_id: &str, now: i64, error: &StoreError) {
        if let Err(record) = self.record_rename_refusal(rename_id, now, error) {
            tracing::error!(rename_id, %record, "recording rename refusal failed");
        }
    }

    /// Record a disk receipt from [`super::execute_rename_disk`] (run without the
    /// Store): persist the destination identity and the re-checked sidecar
    /// decisions in one short transaction, then mark `disk_applied` and drop the
    /// staging link. Replaying a receipt for an already-applied intent is a no-op.
    pub fn record_rename_disk(
        &self,
        receipt: &super::RenameDiskReceipt,
        now: i64,
    ) -> Result<(), StoreError> {
        self.record_rename_disk_observed(receipt, now, || {})
    }

    /// Test seam: `after_decision` runs after the decision commits and before any
    /// sidecar byte is written (a competitor may appear there).
    #[doc(hidden)]
    pub fn record_rename_disk_observed(
        &self,
        receipt: &super::RenameDiskReceipt,
        now: i64,
        after_decision: impl FnOnce(),
    ) -> Result<(), StoreError> {
        let Some(plan) = self.begin_rename_disk_record(receipt)? else {
            return Ok(());
        };
        let decided = self.decide_rename_disk(plan.observe())?;
        after_decision();
        self.finish_rename_disk(decided.apply()?, now)
    }

    /// A refused disk phase (stale or foreign disk state) is terminal: keep the
    /// recovery position for diagnosis and stop retrying. IO errors stay retryable.
    pub fn record_rename_refusal(
        &self,
        rename_id: &str,
        now: i64,
        error: &StoreError,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.record_rename_refusal(rename_id, now, error))
    }

    /// Publish a disk-applied rename in two durable phases.
    ///
    /// `library_published`: one library.db transaction CAS-publishes
    /// path/root/library and every derived fact; the intent stays. Then the
    /// subscribe.db path facts converge idempotently; only when they succeed is
    /// the intent acknowledged (deleted). A reference failure is recorded and
    /// returned, and the intent remains resumable from `library_published`.
    pub fn complete_file_rename(&self, rename_id: &str, now: i64) -> Result<(), StoreError> {
        self.complete_observed_file_rename(
            self.plan_file_rename_completion(rename_id)?.observe()?,
            now,
        )
    }

    /// Publish already-observed bytes with fresh durable phase and version checks.
    pub fn complete_observed_file_rename(
        &self,
        observed: super::rename_completion::ObservedRenameCompletion,
        now: i64,
    ) -> Result<(), StoreError> {
        let super::rename_completion::ObservedRenameCompletion {
            request,
            effective_sidecars,
            reverted_missing_targets,
            source,
            leftovers,
        } = observed;
        if self.load_file_rename(&request.rename_id)? != request {
            return Err(StoreError::Conflict(format!(
                "rename {} changed during completion observation",
                request.rename_id
            )));
        }
        if request.phase == "disk_applied" {
            let source = source.ok_or_else(|| {
                StoreError::Protected("missing rename destination observation".into())
            })?;
            self.publish_rename_library(&request, &effective_sidecars, &source, now)?;
        } else if !reverted_missing_targets.is_empty() {
            self.with_library_write(|db| {
                db.revert_vanished_sidecar_references(
                    &request.file_id.to_string(),
                    &reverted_missing_targets,
                )
            })?;
        }
        self.apply_rename_references(&request, &leftovers, now)
    }

    fn publish_rename_library(
        &self,
        request: &FileRenameRequest,
        effective_sidecars: &[RenameSidecar],
        observed: &library::SourceObservation,
        now: i64,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| {
            db.publish_rename_library(request, effective_sidecars, observed, now)
        })
    }

    /// Converge subscribe.db path facts (idempotent), then acknowledge.
    fn apply_rename_references(
        &self,
        request: &FileRenameRequest,
        leftovers: &[super::rename_completion::ObservedSidecarDeletion],
        now: i64,
    ) -> Result<(), StoreError> {
        let rename_id = request.rename_id.as_str();
        let file_id = request.file_id.to_string();
        let from_str = request.from_path.to_string_lossy();
        let to_str = request.to_path.to_string_lossy();
        if let Err(error) = self.rewrite_fact_paths(request.media_id, &from_str, &to_str) {
            tracing::error!(
                rename_id,
                file_id = %request.file_id,
                from = %from_str,
                to = %to_str,
                %error,
                "rename published in library but subscribe references failed; intent kept"
            );
            self.record_file_rename_failure(rename_id, now, &error.to_string())?;
            return Err(error);
        }
        self.with_writer(crate::Db::Library, |library| {
            set_phase(library, rename_id, "references_applied", now)
        })?;
        // Queue left-over sole-owned source bytes (cross-device copy, identical
        // destination). Shared and retained sources are never queued.
        for leftover in leftovers {
            let path = &leftover.path;
            let path_str = path.to_string_lossy();
            if let Err(error) = self.with_library_write(|db| {
                db.enqueue_file_deletion(crate::pending_file_deletions::EnqueueFileDeletion {
                    path: &path_str,
                    reason: "rename-left-behind-sidecar",
                    scope: crate::FileDeletionScope::ExactPath,
                    file_size: Some(leftover.source_identity.size_bytes),
                    file_device: Some(leftover.source_identity.device),
                    file_inode: Some(leftover.source_identity.inode),
                    file_mtime_ns: Some(leftover.source_identity.mtime_ns),
                    owner: Some(request.from_library_id),
                    require_target: Some((&leftover.target_path, &leftover.target_identity)),
                    companion_file_id: Some(&file_id),
                })
            }) {
                tracing::error!(rename_id, path = %path.display(), %error,
                    "left-behind sidecar enqueue failed; rename intent kept for retry");
                self.record_file_rename_failure(rename_id, now, &error.to_string())?;
                return Err(error);
            }
        }
        self.with_writer(crate::Db::Library, |library| {
            library.execute(
                "DELETE FROM pending_file_renames WHERE rename_id = ?1",
                params![rename_id],
            )?;
            Ok(())
        })
    }

    /// Drive every unfinished rename from its last durable phase.
    /// `now` is unix seconds (the unit of `next_retry_at`); only due intents run.
    pub fn resume_pending_file_renames(&self, now: i64) -> Result<usize, StoreError> {
        let ids = self.list_due_file_rename_ids(now)?;
        let mut done = 0usize;
        for id in ids {
            match self.resume_one_file_rename(&id, now) {
                Ok(()) => done += 1,
                Err(error) => {
                    if let Err(record) =
                        self.record_file_rename_failure(&id, now, &error.to_string())
                    {
                        tracing::error!(rename_id = %id, %record, "recording rename failure failed");
                    }
                    tracing::error!(rename_id = %id, %error, "pending file rename resume failed");
                }
            }
        }
        Ok(done)
    }

    pub fn resume_one_file_rename(&self, rename_id: &str, now: i64) -> Result<(), StoreError> {
        let request = self
            .load_file_rename(rename_id)
            .inspect_err(|error| self.log_rename_refusal(rename_id, now, error))?;
        let _publication = match request.phase.as_str() {
            "planned" => Some(self.apply_file_rename_disk(&request, now)?),
            "disk_applied" => Some(
                self.admit_rename_publication(&request)
                    .inspect_err(|error| self.log_rename_refusal(rename_id, now, error))?,
            ),
            _ => None,
        };
        self.complete_file_rename(rename_id, now)
            .inspect_err(|error| self.log_rename_refusal(rename_id, now, error))
    }

    /// Unfinished intents whose retry time (unix seconds) has come.
    pub fn list_due_file_rename_ids(&self, now: i64) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.list_due_file_rename_ids(now))
    }

    /// Capture the startup recovery set, including intents still in backoff.
    pub fn file_rename_recovery_schedule(&self) -> Result<Vec<(i64, String)>, StoreError> {
        self.with_library_read(|db| db.file_rename_recovery_schedule())
    }

    /// Look up the latest persisted retry time for one already captured intent.
    pub fn file_rename_retry_at(&self, id: &str) -> Result<Option<i64>, StoreError> {
        self.with_library_read(|db| db.file_rename_retry_at(id))
    }

    /// A bounded recovery window; release the Store between windows and disk work.
    pub fn list_due_file_rename_ids_limited(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.list_due_file_rename_ids_limited(now, limit))
    }

    pub fn load_file_rename(&self, rename_id: &str) -> Result<FileRenameRequest, StoreError> {
        self.with_library_read(|db| db.load_file_rename(rename_id))
    }

    pub fn record_file_rename_failure(
        &self,
        rename_id: &str,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.record_file_rename_failure(rename_id, now, error))
    }
}

pub(crate) struct DestOwner {
    pub library_id: String,
    pub root_id: String,
}

pub(super) fn refuse_occupied(
    tx: &Connection,
    path: &str,
    file_id: &str,
) -> Result<(), StoreError> {
    let occupied: i64 = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files
                       WHERE path_key = norm_path_key(?1) AND file_id <> ?2)",
        params![path, file_id],
        |r| r.get(0),
    )?;
    if occupied != 0 {
        return Err(StoreError::Protected(format!("{path} is already recorded")));
    }
    Ok(())
}

pub(super) fn observe_sidecars(from: &Path, to: &Path) -> Vec<RenameSidecar> {
    let mut out = Vec::new();
    let Some(from_stem) = from.file_stem().and_then(|s| s.to_str()) else {
        return out;
    };
    let Some(to_stem) = to.file_stem().and_then(|s| s.to_str()) else {
        return out;
    };
    let Some(from_parent) = from.parent() else {
        return out;
    };
    let Some(to_parent) = to.parent() else {
        return out;
    };
    // Same-stem NFO beside the video.
    let from_nfo = from_parent.join(format!("{from_stem}.nfo"));
    if from_nfo.is_file() {
        let source_identity = SidecarSourceIdentity::observe(&from_nfo);
        out.push(RenameSidecar {
            kind: "nfo".into(),
            from: from_nfo,
            to: to_parent.join(format!("{to_stem}.nfo")),
            action: SidecarAction::Move,
            source_identity,
            target_identity: None,
        });
    }
    out
}

pub(super) fn sidecars_json(sidecars: &[RenameSidecar]) -> Result<String, StoreError> {
    serde_json::to_string(sidecars).map_err(|e| StoreError::Invalid(format!("sidecars json: {e}")))
}

pub(super) fn ensure_rename_libraries_admitted(
    tx: &Connection,
    request: &FileRenameRequest,
    already_moved: bool,
) -> Result<(), StoreError> {
    let from = request.from_library_id.to_string();
    let to = request.to_library_id.to_string();
    let mut revision_mismatch = false;
    for (library, expected_revision) in [
        (&from, request.from_config_revision),
        (&to, request.to_config_revision),
    ] {
        let removing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_removals
                           WHERE library_id = ?1 AND state <> 'succeeded')",
            params![library],
            |r| r.get(0),
        )?;
        if removing {
            return Err(StoreError::Protected(format!(
                "library {library} is being removed"
            )));
        }
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT lifecycle_state, config_revision FROM libraries WHERE library_id = ?1",
                params![library],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((lifecycle, current_revision)) = row else {
            return Err(StoreError::Missing(format!("library {library}")));
        };
        if lifecycle != "ready" {
            return Err(StoreError::Conflict(format!(
                "library {library} lifecycle is {lifecycle}"
            )));
        }
        if current_revision != expected_revision {
            revision_mismatch = true;
        }
    }
    if revision_mismatch {
        if !already_moved {
            return Err(StoreError::Protected(format!(
                "rename {}: Library configuration changed since planning",
                request.rename_id
            )));
        }
        let to_str = request.to_path.to_string_lossy();
        let root_valid: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM library_roots
                 WHERE library_id = ?1 AND root_id = ?2 AND enabled = 1
                   AND (path_key = '/'
                        OR norm_path_key(?3) = path_key
                        OR substr(norm_path_key(?3), 1, length(path_key) + 1) = path_key || '/')
             )",
            params![to, request.to_root_id, to_str.as_ref()],
            |r| r.get(0),
        )?;
        if !root_valid {
            return Err(StoreError::Protected(format!(
                "rename {}: destination root {} no longer owns {}",
                request.rename_id,
                request.to_root_id,
                request.to_path.display()
            )));
        }
    }
    Ok(())
}

/// Resolve which Library/root owns `to`. Allows same-Library cross-root and
/// cross-Library destinations; rejects nested child ownership and symlink escape
/// outside the destination root.
pub(super) fn resolve_destination_owner(
    tx: &Connection,
    owned: &Owned,
    to: &Path,
) -> Result<DestOwner, StoreError> {
    let key = crate::path_key::normalize_path_key(&to.to_string_lossy());
    let mut statement = tx.prepare("SELECT r.root_id,r.library_id,r.path_key FROM library_roots r JOIN libraries l ON l.library_id=r.library_id WHERE r.enabled=1 AND l.kind=?1")?;
    let roots: Vec<(String, String, String)> = statement
        .query_map([&owned.kind], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut best: Option<(DestOwner, usize)> = None;
    for (root_id, library_id, root) in roots {
        if !Path::new(&key).starts_with(Path::new(&root)) {
            continue;
        }
        let depth = Path::new(&root).components().count();
        if best.as_ref().is_none_or(|(_, previous)| depth > *previous) {
            best = Some((
                DestOwner {
                    library_id,
                    root_id,
                },
                depth,
            ));
        }
    }
    best.map(|(owner, _)| owner).ok_or_else(|| {
        StoreError::Protected(format!(
            "{} is outside any writable Library root (nested/symlink rejected)",
            to.display()
        ))
    })
}

#[path = "rename_durable/connection.rs"]
mod connection;
#[path = "rename_durable/sidecar_refs.rs"]
mod sidecar_refs;
pub(super) use sidecar_refs::merge_registered_sidecars;
use sidecar_refs::{migrate_metadata_inputs, migrate_scrape_sidecars, rewrite_artwork_paths};
