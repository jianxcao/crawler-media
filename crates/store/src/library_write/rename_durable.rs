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

use super::publish::{ensure_library_writable, ensure_media_writable};
use super::reduce::reduce_library_media;
use super::rename::{Owned, load_owned};
pub use super::rename_disk::RenameSourceEvidence;
use super::rename_record::{RawRename, config_revision, set_phase};
pub use super::rename_sidecars::SidecarAction;
use crate::{Store, StoreError};

/// One sidecar that must move with the video (NFO / scrape-registered path).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSidecar {
    pub kind: String,
    pub from: PathBuf,
    pub to: PathBuf,
    /// Durable decision; see [`SidecarAction`].
    pub action: SidecarAction,
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

impl Store {
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
            self.publish_rename_library(&request, &source, now)?;
        }
        self.apply_rename_references(&request, &leftovers, now)
    }

    fn publish_rename_library(
        &self,
        request: &FileRenameRequest,
        observed: &library::SourceObservation,
        now: i64,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.publish_rename_library(request, observed, now))
    }

    /// Converge subscribe.db path facts (idempotent), then acknowledge.
    fn apply_rename_references(
        &self,
        request: &FileRenameRequest,
        leftovers: &[super::rename_completion::ObservedSidecarDeletion],
        now: i64,
    ) -> Result<(), StoreError> {
        let rename_id = request.rename_id.as_str();
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
            if let Err(error) = self.enqueue_file_deletion_observed(
                &path.to_string_lossy(),
                "rename-left-behind-sidecar",
                leftover.identity,
            ) {
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

    fn resume_one_file_rename(&self, rename_id: &str, now: i64) -> Result<(), StoreError> {
        let request = self.load_file_rename(rename_id)?;
        if request.phase == "planned" {
            // Startup recovery: the disk phase is idempotent and bounded to one file.
            let receipt = super::execute_rename_disk(&request).inspect_err(|error| {
                let _ = self.record_rename_refusal(rename_id, now, error);
            })?;
            self.record_rename_disk(&receipt, now)?;
        }
        self.complete_file_rename(rename_id, now)
    }

    /// Unfinished intents whose retry time (unix seconds) has come.
    pub fn list_due_file_rename_ids(&self, now: i64) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.list_due_file_rename_ids(now))
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
        out.push(RenameSidecar {
            kind: "nfo".into(),
            from: from_nfo,
            to: to_parent.join(format!("{to_stem}.nfo")),
            action: SidecarAction::Move,
        });
    }
    out
}

pub(super) fn sidecars_json(sidecars: &[RenameSidecar]) -> Result<String, StoreError> {
    serde_json::to_string(sidecars).map_err(|e| StoreError::Invalid(format!("sidecars json: {e}")))
}

fn migrate_metadata_inputs(
    tx: &Connection,
    from_library: &str,
    to_library: &str,
    to_root: &str,
    from_video: &str,
    to_video: &str,
    sidecars: &[RenameSidecar],
) -> Result<(), StoreError> {
    let mut pairs: Vec<(String, String)> = vec![(from_video.to_string(), to_video.to_string())];
    for side in sidecars {
        pairs.push((
            side.from.to_string_lossy().into_owned(),
            side.to.to_string_lossy().into_owned(),
        ));
    }
    for (from, to) in pairs {
        if from_library == to_library {
            tx.execute(
                "UPDATE library_metadata_inputs
                 SET path_key = norm_path_key(?2), root_id = ?3, revision = revision + 1
                 WHERE library_id = ?4 AND path_key = norm_path_key(?1)",
                params![from, to, to_root, from_library],
            )?;
        } else {
            // PK includes library_id: copy then delete so cross-Library re-parent
            // keeps the input under the destination Library.
            tx.execute(
                "INSERT INTO library_metadata_inputs
                    (library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,
                     parse_state,source_version,fields_json,present_fields_json,provenance_json,
                     export_id,export_digest,export_fields_json,revision)
                 SELECT ?2,media_id,entity_scope,season,episode,locale,input_key,?3,norm_path_key(?4),
                        parse_state,source_version,fields_json,present_fields_json,provenance_json,
                        export_id,export_digest,export_fields_json,revision+1
                 FROM library_metadata_inputs
                 WHERE library_id = ?1 AND path_key = norm_path_key(?5)
                 ON CONFLICT DO UPDATE SET
                    root_id=excluded.root_id, path_key=excluded.path_key,
                    revision=library_metadata_inputs.revision+1",
                params![from_library, to_library, to_root, to, from],
            )?;
            tx.execute(
                "DELETE FROM library_metadata_inputs WHERE library_id = ?1 AND path_key = norm_path_key(?2)",
                params![from_library, from],
            )?;
        }
    }
    Ok(())
}

pub(super) fn merge_registered_sidecars(
    rows: &[(String, String)],
    from_video: &Path,
    to_video: &Path,
    sidecars: &mut Vec<RenameSidecar>,
) {
    let Some(from_parent) = from_video.parent() else {
        return;
    };
    let Some(to_parent) = to_video.parent() else {
        return;
    };
    let from_stem = from_video
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let to_stem = to_video.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    for (kind, path) in rows {
        let from = PathBuf::from(path);
        if sidecars.iter().any(|s| s.from == from) {
            continue;
        }
        if !from.is_file() {
            continue;
        }
        // Only move sidecars that live next to the video or share its stem.
        let same_dir = from.parent() == Some(from_parent);
        let shares_stem = from
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| stem == from_stem);
        if !(same_dir || shares_stem) {
            continue;
        }
        let name = from.file_name().map(|n| n.to_os_string());
        let Some(name) = name else { continue };
        let to = if shares_stem {
            let ext = from.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.is_empty() {
                to_parent.join(to_stem)
            } else {
                to_parent.join(format!("{to_stem}.{ext}"))
            }
        } else {
            to_parent.join(name)
        };
        sidecars.push(RenameSidecar {
            kind: kind.clone(),
            from,
            to,
            action: SidecarAction::Move,
        });
    }
}

fn migrate_scrape_sidecars(
    tx: &Connection,
    file_id: &str,
    sidecars: &[RenameSidecar],
) -> Result<(), StoreError> {
    for side in sidecars {
        let from = side.from.to_string_lossy().into_owned();
        let to = side.to.to_string_lossy().into_owned();
        tx.execute(
            "UPDATE scrape_sidecars SET path = ?3 WHERE file_id = ?1 AND path = ?2",
            params![file_id, from, to],
        )?;
    }
    Ok(())
}

fn rewrite_artwork_paths(
    tx: &Connection,
    file_id: &str,
    sidecars: &[RenameSidecar],
) -> Result<(), StoreError> {
    for side in sidecars {
        let from = side.from.to_string_lossy().into_owned();
        let to = side.to.to_string_lossy().into_owned();
        tx.execute(
            "UPDATE library_files SET
                poster_path = CASE WHEN poster_path = ?2 THEN ?3 ELSE poster_path END,
                backdrop_path = CASE WHEN backdrop_path = ?2 THEN ?3 ELSE backdrop_path END,
                still_path = CASE WHEN still_path = ?2 THEN ?3 ELSE still_path END
             WHERE file_id = ?1",
            params![file_id, from, to],
        )?;
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
