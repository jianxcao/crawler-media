//! Durable file rename / re-parent with sidecar intents and crash recovery.
//!
//! Same-root rename, same-Library cross-root re-parent, and cross-Library
//! re-parent all share one persisted intent: observe sidecars outside the
//! Store lock, register the plan, move bytes with the lock released, then CAS
//! publish path/root/library + derived facts. A crash between any of those
//! steps leaves a resumable `pending_file_renames` row.

use std::path::{Path, PathBuf};

use domain::{FileVersion, LedgerId, LibraryId, MediaId, MediaKind};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::publish::{ensure_library_writable, ensure_media_writable};
use super::reduce::reduce_library_media;
use super::rename::{Owned, load_owned};
pub use super::rename_disk::RenameSourceEvidence;
use super::rename_record::{RawRename, config_revision, set_phase};
pub use super::rename_sidecars::SidecarMode;
use super::rename_sidecars::{
    apply_sidecar, classify_shared_sidecars, observe_directory_artwork, recheck_for_execution,
};
use crate::library_paths::resolved_path;
use crate::{Store, StoreError};

/// One sidecar that must move with the video (NFO / scrape-registered path).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSidecar {
    pub kind: String,
    pub from: PathBuf,
    pub to: PathBuf,
    /// Move when this video is the sole owner, copy when other media share it.
    pub mode: SidecarMode,
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
    /// Observe sidecars beside `from` (lock-free) and persist a rename intent.
    ///
    /// Destination ownership is decided here: same root, same-Library other
    /// root, or another Library. Nested child Libraries and symlink escapes are
    /// rejected. Disk bytes are not touched.
    pub fn begin_file_rename(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
        now: i64,
    ) -> Result<FileRenameRequest, StoreError> {
        self.begin_rename_observed_at(file_id, expected, from, to, None, now)
    }

    /// Ledger-only reconciliation: the bytes already sit at `to` and `from` is gone.
    /// Evidence is observed at `to`, so the disk phase verifies the moved inode
    /// instead of moving anything.
    pub fn begin_file_reconciliation(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
        now: i64,
    ) -> Result<FileRenameRequest, StoreError> {
        if std::fs::symlink_metadata(from).is_ok() {
            return Err(StoreError::Protected(format!(
                "reconciliation source {} still exists",
                from.display()
            )));
        }
        self.begin_rename_observed_at(file_id, expected, from, to, Some(to), now)
    }

    fn begin_rename_observed_at(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        from: &Path,
        to: &Path,
        moved_to: Option<&Path>,
        now: i64,
    ) -> Result<FileRenameRequest, StoreError> {
        if !from.is_absolute() || !to.is_absolute() {
            return Err(StoreError::Protected(
                "rename paths must be absolute".into(),
            ));
        }
        let mut sidecars = observe_sidecars(from, to);
        observe_directory_artwork(from, to, &mut sidecars);
        // Native identity of the planned source, observed before the transaction.
        let at = moved_to.unwrap_or(from);
        let evidence = RenameSourceEvidence::observe(at, expected.clone())?;
        // A reconciled move changes ctime, so its recorded version cannot match at
        // `to`; the moved inode at `to` is the evidence instead.
        if moved_to.is_none()
            && library::observe_source(from)
                .map_err(|error| {
                    StoreError::Protected(format!("rename source {}: {error}", from.display()))
                })?
                .source_version
                != expected.source_version
        {
            return Err(StoreError::Protected(format!(
                "file {file_id} on disk no longer matches its recorded version"
            )));
        }
        let tx = crate::write_tx(&self.library)?;
        let id = file_id.to_string();
        let from_str = from.to_string_lossy().into_owned();
        let owned = load_owned(&tx, &id, &from_str)?;
        merge_registered_sidecars(&tx, &id, from, to, &mut sidecars)?;
        classify_shared_sidecars(&tx, &id, from, &mut sidecars)?;
        if owned.revision != expected.revision || owned.source_version != expected.source_version {
            return Err(StoreError::Protected(format!("file {id} version changed")));
        }
        ensure_library_writable(&tx, &owned.library_id)?;
        let media_id: MediaId = owned.media_id.parse()?;
        ensure_media_writable(&tx, media_id)?;
        let dest = resolve_destination_owner(self, &tx, &owned, to)?;
        ensure_library_writable(&tx, &dest.library_id)?;
        let to_str = to.to_string_lossy().into_owned();
        refuse_occupied(&tx, &to_str, &id)?;
        let from_config_revision = config_revision(&tx, &owned.library_id)?;
        let to_config_revision = config_revision(&tx, &dest.library_id)?;
        // Destination sidecar targets must also be free of recorded files.
        for side in &sidecars {
            let side_to = side.to.to_string_lossy().into_owned();
            refuse_occupied(&tx, &side_to, &id)?;
        }
        let rename_id = uuid::Uuid::new_v4().to_string();
        let sidecars_json = serde_json::to_string(&sidecars)
            .map_err(|e| StoreError::Invalid(format!("sidecars json: {e}")))?;
        tx.execute(
            "INSERT INTO pending_file_renames
                (rename_id, file_id, from_path, to_path, expected_revision, expected_source_version,
                 from_library_id, from_root_id, to_library_id, to_root_id, sidecars_json, phase,
                 source_size, source_device, source_inode, source_mtime_ns,
                 from_config_revision, to_config_revision, created_at_ms, updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'planned',?12,?13,?14,?15,?16,?17,?18,?18)",
            params![
                rename_id,
                id,
                from_str,
                to_str,
                owned.revision,
                owned.source_version,
                owned.library_id,
                owned.root_id,
                dest.library_id,
                dest.root_id,
                sidecars_json,
                evidence.size_bytes as i64,
                evidence.device as i64,
                evidence.inode as i64,
                evidence.mtime_ns,
                from_config_revision,
                to_config_revision,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(FileRenameRequest {
            rename_id,
            file_id,
            expected: expected.clone(),
            from_path: from.to_path_buf(),
            to_path: to.to_path_buf(),
            from_library_id: owned.library_id.parse()?,
            from_root_id: owned.root_id,
            to_library_id: dest.library_id.parse()?,
            to_root_id: dest.root_id,
            sidecars,
            phase: "planned".into(),
            media_id,
            source_evidence: evidence,
            disk_receipt: None,
            from_config_revision,
            to_config_revision,
        })
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
        let rename_id = receipt.rename_id.as_str();
        let request = self.load_file_rename(rename_id)?;
        match request.phase.as_str() {
            "planned" => {}
            "disk_applied" | "library_published" | "references_applied" => return Ok(()),
            phase => {
                return Err(StoreError::Protected(format!(
                    "rename {rename_id} is {phase}; disk receipt refused"
                )));
            }
        }
        let destination = &receipt.destination;
        let mut sidecars = request.sidecars.clone();
        let tx = crate::write_tx(&self.library)?;
        recheck_for_execution(
            &tx,
            &request.file_id.to_string(),
            &request.from_path,
            &mut sidecars,
        )?;
        let sidecars_json = serde_json::to_string(&sidecars)
            .map_err(|e| StoreError::Invalid(format!("sidecars json: {e}")))?;
        tx.execute(
            "UPDATE pending_file_renames
             SET sidecars_json = ?2, dest_size = ?3, dest_device = ?4, dest_inode = ?5,
                 dest_mtime_ns = ?6
             WHERE rename_id = ?1 AND phase = 'planned'",
            params![
                rename_id,
                sidecars_json,
                destination.size_bytes as i64,
                destination.device as i64,
                destination.inode as i64,
                destination.mtime_ns
            ],
        )?;
        tx.commit()?;
        apply_sidecar_moves(&sidecars)?;
        self.mark_rename_disk_applied(rename_id, now)?;
        super::rename_receipt::remove_staging(&request)
    }

    /// A refused disk phase (stale or foreign disk state) is terminal: keep the
    /// recovery position for diagnosis and stop retrying. IO errors stay retryable.
    pub fn record_rename_refusal(
        &self,
        rename_id: &str,
        now: i64,
        error: &StoreError,
    ) -> Result<(), StoreError> {
        if matches!(error, StoreError::Protected(_) | StoreError::Missing(_)) {
            self.mark_file_rename_failed(rename_id, now, &error.to_string())?;
        }
        Ok(())
    }

    /// Publish a disk-applied rename in two durable phases.
    ///
    /// `library_published`: one library.db transaction CAS-publishes
    /// path/root/library and every derived fact; the intent stays. Then the
    /// subscribe.db path facts converge idempotently; only when they succeed is
    /// the intent acknowledged (deleted). A reference failure is recorded and
    /// returned, and the intent remains resumable from `library_published`.
    pub fn complete_file_rename(&self, rename_id: &str, now: i64) -> Result<(), StoreError> {
        let request = self.load_file_rename(rename_id)?;
        match request.phase.as_str() {
            "disk_applied" => self.publish_rename_library(&request, now)?,
            "library_published" | "references_applied" => {}
            phase => {
                return Err(StoreError::Protected(format!(
                    "rename {rename_id} phase {phase} cannot complete; apply disk first"
                )));
            }
        }
        self.apply_rename_references(&request, now)
    }

    fn publish_rename_library(
        &self,
        request: &FileRenameRequest,
        now: i64,
    ) -> Result<(), StoreError> {
        let rename_id = request.rename_id.as_str();
        if !request.to_path.is_file() {
            return Err(StoreError::Protected(format!(
                "rename {rename_id}: destination {} missing",
                request.to_path.display()
            )));
        }
        // Content-token verification: one stat (one read for .strm) before the
        // transaction. A rename keeps the token; a copy (cross-filesystem move)
        // or changed bytes yield a new one and must advance the FileVersion.
        let observed = library::observe_source(&request.to_path).map_err(|error| {
            StoreError::Protected(format!("rename {rename_id}: observe destination: {error}"))
        })?;
        let tx = crate::write_tx(&self.library)?;
        let id = request.file_id.to_string();
        let from_str = request.from_path.to_string_lossy().into_owned();
        let to_str = request.to_path.to_string_lossy().into_owned();
        let owned = load_owned(&tx, &id, &from_str)?;
        if owned.revision != request.expected.revision
            || owned.source_version != request.expected.source_version
        {
            return Err(StoreError::Protected(format!(
                "file {id} version changed during rename"
            )));
        }
        let from_library = request.from_library_id.to_string();
        let to_library = request.to_library_id.to_string();
        if config_revision(&tx, &from_library)? != request.from_config_revision
            || config_revision(&tx, &to_library)? != request.to_config_revision
        {
            return Err(StoreError::Protected(format!(
                "rename {rename_id}: Library configuration changed since planning"
            )));
        }
        ensure_library_writable(&tx, &from_library)?;
        ensure_library_writable(&tx, &to_library)?;
        ensure_media_writable(&tx, request.media_id)?;
        refuse_occupied(&tx, &to_str, &id)?;
        let changed = tx.execute(
            "UPDATE library_files
             SET path_key = norm_path_key(?2), display_path = ?2, root_id = ?3, library_id = ?4
             WHERE file_id = ?1 AND file_revision = ?5 AND source_version = ?6",
            params![
                id,
                to_str,
                request.to_root_id,
                to_library,
                owned.revision,
                owned.source_version,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Protected(format!(
                "file {id} changed during rename publish"
            )));
        }
        if observed.source_version != owned.source_version {
            super::source::advance_to_new_content(
                &tx,
                request.file_id,
                owned.revision + 1,
                &observed,
                now,
            )?;
        }
        // Coverage rows carry composite FK (file_id, library_id, media_id).
        tx.execute(
            "UPDATE library_file_units SET library_id = ?2 WHERE file_id = ?1",
            params![id, to_library],
        )?;
        migrate_metadata_inputs(
            &tx,
            &from_library,
            &to_library,
            &request.to_root_id,
            &from_str,
            &to_str,
            &request.sidecars,
        )?;
        migrate_scrape_sidecars(&tx, &id, &request.sidecars)?;
        rewrite_artwork_paths(&tx, &id, &request.sidecars)?;
        reduce_library_media(&tx, request.from_library_id, request.media_id)?;
        if request.to_library_id != request.from_library_id {
            reduce_library_media(&tx, request.to_library_id, request.media_id)?;
        }
        set_phase(&tx, rename_id, "library_published", now)?;
        tx.commit()?;
        Ok(())
    }

    /// Converge subscribe.db path facts (idempotent), then acknowledge.
    fn apply_rename_references(
        &self,
        request: &FileRenameRequest,
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
        set_phase(&self.library, rename_id, "references_applied", now)?;
        // Queue leftover source-side sidecar bytes (rename failed / copy case).
        for side in &request.sidecars {
            if side.mode == SidecarMode::Move && side.from.is_file() && side.from != side.to {
                if let Err(error) = self.enqueue_file_deletion(
                    &side.from.to_string_lossy(),
                    "rename-left-behind-sidecar",
                ) {
                    tracing::warn!(rename_id, path = %side.from.display(), %error, "enqueue left-behind sidecar failed");
                }
            }
        }
        self.library.execute(
            "DELETE FROM pending_file_renames WHERE rename_id = ?1",
            params![rename_id],
        )?;
        Ok(())
    }

    /// Drive every unfinished rename from its last durable phase.
    pub fn resume_pending_file_renames(&self, now: i64) -> Result<usize, StoreError> {
        let ids = self.list_pending_file_rename_ids()?;
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

    pub fn list_pending_file_rename_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT rename_id FROM pending_file_renames
             WHERE phase IN ('planned','disk_applied','library_published','references_applied')
             ORDER BY created_at_ms, rename_id",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn load_file_rename(&self, rename_id: &str) -> Result<FileRenameRequest, StoreError> {
        let raw = self
            .library
            .query_row(
                "SELECT r.file_id, r.from_path, r.to_path, r.expected_revision,
                        r.expected_source_version, r.from_library_id, r.from_root_id,
                        r.to_library_id, r.to_root_id, r.sidecars_json, r.phase,
                        r.source_size, r.source_device, r.source_inode, r.source_mtime_ns,
                        r.from_config_revision, r.to_config_revision, f.media_id,
                        r.dest_size, r.dest_device, r.dest_inode, r.dest_mtime_ns
                 FROM pending_file_renames r JOIN library_files f ON f.file_id = r.file_id
                 WHERE r.rename_id = ?1",
                params![rename_id],
                RawRename::read,
            )
            .optional()?
            .ok_or_else(|| StoreError::Missing(format!("rename {rename_id}")))?;
        raw.into_request(rename_id)
    }

    fn mark_file_rename_failed(
        &self,
        rename_id: &str,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        tracing::error!(rename_id, error, "rename refused; intent marked failed");
        self.library.execute(
            "UPDATE pending_file_renames
             SET last_completed_phase = phase, phase = 'failed', last_error = ?3,
                 updated_at_ms = ?2
             WHERE rename_id = ?1 AND phase <> 'failed'",
            params![rename_id, now, error],
        )?;
        Ok(())
    }

    fn mark_rename_disk_applied(&self, rename_id: &str, now: i64) -> Result<(), StoreError> {
        let n = self.library.execute(
            "UPDATE pending_file_renames
             SET phase = 'disk_applied', last_completed_phase = 'disk_applied',
                 updated_at_ms = ?2, last_error = NULL
             WHERE rename_id = ?1 AND phase IN ('planned','disk_applied')",
            params![rename_id, now],
        )?;
        if n != 1 {
            return Err(StoreError::Protected(format!(
                "rename {rename_id} missing while marking disk_applied"
            )));
        }
        Ok(())
    }

    fn record_file_rename_failure(
        &self,
        rename_id: &str,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE pending_file_renames
             SET attempts = attempts + 1,
                 next_retry_at = ?2 + MIN(86400, 60 * (1 << MIN(attempts, 10))),
                 last_error = ?3,
                 updated_at_ms = ?2
             WHERE rename_id = ?1",
            params![rename_id, now, error],
        )?;
        Ok(())
    }
}

pub(crate) struct DestOwner {
    pub library_id: String,
    pub root_id: String,
}

fn refuse_occupied(tx: &Connection, path: &str, file_id: &str) -> Result<(), StoreError> {
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

fn observe_sidecars(from: &Path, to: &Path) -> Vec<RenameSidecar> {
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
            mode: SidecarMode::Move,
        });
    }
    out
}

fn apply_sidecar_moves(sidecars: &[RenameSidecar]) -> Result<(), StoreError> {
    // Already moved / copied, or never present after a crash — skipped inside.
    for side in sidecars {
        apply_sidecar(side)?;
    }
    Ok(())
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

fn merge_registered_sidecars(
    tx: &Connection,
    file_id: &str,
    from_video: &Path,
    to_video: &Path,
    sidecars: &mut Vec<RenameSidecar>,
) -> Result<(), StoreError> {
    let mut stmt = tx.prepare("SELECT kind, path FROM scrape_sidecars WHERE file_id = ?1")?;
    let rows = stmt
        .query_map(params![file_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let Some(from_parent) = from_video.parent() else {
        return Ok(());
    };
    let Some(to_parent) = to_video.parent() else {
        return Ok(());
    };
    let from_stem = from_video
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let to_stem = to_video.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    for (kind, path) in rows {
        let from = PathBuf::from(&path);
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
                to_parent.join(format!("{to_stem}"))
            } else {
                to_parent.join(format!("{to_stem}.{ext}"))
            }
        } else {
            to_parent.join(name)
        };
        sidecars.push(RenameSidecar {
            kind,
            from,
            to,
            mode: SidecarMode::Move,
        });
    }
    Ok(())
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
pub(crate) fn resolve_destination_owner(
    store: &Store,
    tx: &Connection,
    owned: &Owned,
    to: &Path,
) -> Result<DestOwner, StoreError> {
    let outside = || {
        StoreError::Protected(format!(
            "{} is outside any writable Library root (nested/symlink rejected)",
            to.display()
        ))
    };
    if !to.is_absolute() {
        return Err(outside());
    }
    let kind: MediaKind = owned
        .kind
        .parse()
        .map_err(|_| StoreError::Invalid(format!("kind {}", owned.kind)))?;
    let Some(library) = store.library_for_path_strict(to, kind)? else {
        return Err(outside());
    };
    let resolved_to = resolved_path(to)?;
    // Pick the longest matching root of that Library.
    let roots: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT root_id, display_path FROM library_roots WHERE library_id = ?1 AND enabled = 1",
        )?;
        stmt.query_map(params![library.id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut best: Option<(String, usize)> = None;
    for (root_id, display) in &roots {
        let root_resolved = resolved_path(Path::new(display))?;
        if resolved_to.starts_with(&root_resolved) {
            let depth = root_resolved.components().count();
            if best.as_ref().is_none_or(|(_, d)| depth > *d) {
                best = Some((root_id.clone(), depth));
            }
        }
    }
    let Some((root_id, _)) = best else {
        return Err(outside());
    };
    Ok(DestOwner {
        library_id: library.id,
        root_id,
    })
}
