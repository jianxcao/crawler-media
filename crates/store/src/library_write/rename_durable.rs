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
pub use super::rename_sidecars::SidecarMode;
use super::rename_sidecars::{
    apply_sidecar, classify_shared_sidecars, observe_directory_artwork, recheck_for_execution,
};
use crate::library_paths::resolved_path;
use crate::pending_file_deletions::file_identity;
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
        if !from.is_absolute() || !to.is_absolute() {
            return Err(StoreError::Protected(
                "rename paths must be absolute".into(),
            ));
        }
        let mut sidecars = observe_sidecars(from, to);
        observe_directory_artwork(from, to, &mut sidecars);
        let identity = file_identity(&from.to_string_lossy());
        let tx = self.library.unchecked_transaction()?;
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
                 file_size, file_inode, created_at_ms, updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'planned',?12,?13,?14,?14)",
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
                identity.map(|(size, _)| size as i64),
                identity.and_then(|(_, ino)| ino).map(|ino| ino as i64),
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
        })
    }

    /// Move video + sidecar bytes for a planned rename. Safe to call with the
    /// Store lock released. Marks the intent `disk_applied` when the video is
    /// at `to` (including the crash case where bytes already moved).
    pub fn apply_file_rename_disk(&self, rename_id: &str, now: i64) -> Result<(), StoreError> {
        let request = self.load_file_rename(rename_id)?;
        if request.phase == "disk_applied" {
            return Ok(());
        }
        move_rename_bytes(&request)?;
        // Sidecars move here, under the Store lock, after re-checking ownership so a
        // file published since `begin` cannot lose shared artwork.
        let mut sidecars = request.sidecars.clone();
        let tx = self.library.unchecked_transaction()?;
        recheck_for_execution(
            &tx,
            &request.file_id.to_string(),
            &request.from_path,
            &mut sidecars,
        )?;
        let sidecars_json = serde_json::to_string(&sidecars)
            .map_err(|e| StoreError::Invalid(format!("sidecars json: {e}")))?;
        tx.execute(
            "UPDATE pending_file_renames SET sidecars_json = ?2 WHERE rename_id = ?1",
            params![rename_id, sidecars_json],
        )?;
        tx.commit()?;
        apply_sidecar_moves(&sidecars)?;
        self.mark_rename_disk_applied(rename_id, now)?;
        Ok(())
    }

    /// CAS-publish path/root/library, migrate NFO inputs and scrape sidecars,
    /// rewrite subscribe facts, reduce affected Library cards, drop the intent.
    pub fn complete_file_rename(&self, rename_id: &str, _now: i64) -> Result<(), StoreError> {
        let request = self.load_file_rename(rename_id)?;
        if request.phase != "disk_applied" && request.phase != "planned" {
            return Err(StoreError::Protected(format!(
                "rename {rename_id} phase {} cannot complete",
                request.phase
            )));
        }
        // Require the video to sit at the destination before publishing.
        if !request.to_path.is_file() {
            return Err(StoreError::Protected(format!(
                "rename {rename_id}: destination {} missing; apply disk first",
                request.to_path.display()
            )));
        }
        let tx = self.library.unchecked_transaction()?;
        let id = request.file_id.to_string();
        let from_str = request.from_path.to_string_lossy().into_owned();
        let to_str = request.to_path.to_string_lossy().into_owned();
        // Row may already show `to` if a previous complete partially ran; accept either.
        let owned = match load_owned(&tx, &id, &from_str) {
            Ok(owned) => owned,
            Err(_) => load_owned(&tx, &id, &to_str)?,
        };
        if owned.revision != request.expected.revision
            || owned.source_version != request.expected.source_version
        {
            // Already published by a prior complete (revision bumped).
            if owned.library_id == request.to_library_id.to_string()
                && (owned.root_id == request.to_root_id)
            {
                let path: String = tx.query_row(
                    "SELECT path_key FROM library_files WHERE file_id = ?1",
                    params![id],
                    |r| r.get(0),
                )?;
                if path == crate::path_key::normalize_path_key(&to_str) {
                    tx.execute(
                        "DELETE FROM pending_file_renames WHERE rename_id = ?1",
                        params![rename_id],
                    )?;
                    tx.commit()?;
                    return Ok(());
                }
            }
            return Err(StoreError::Protected(format!(
                "file {id} version changed during rename"
            )));
        }
        ensure_library_writable(&tx, &request.from_library_id.to_string())?;
        ensure_library_writable(&tx, &request.to_library_id.to_string())?;
        ensure_media_writable(&tx, request.media_id)?;
        refuse_occupied(&tx, &to_str, &id)?;

        let display = to_str.clone();
        // path_key uses the same absolute display spelling the caller supplied;
        // resolved form is only used for ownership checks (see ensure_*).
        let changed = tx.execute(
            "UPDATE library_files
             SET path_key = norm_path_key(?2), display_path = ?2, root_id = ?3, library_id = ?4
             WHERE file_id = ?1 AND file_revision = ?5 AND source_version = ?6",
            params![
                id,
                display,
                request.to_root_id,
                request.to_library_id.to_string(),
                owned.revision,
                owned.source_version,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Protected(format!(
                "file {id} changed during rename publish"
            )));
        }
        // Coverage rows carry composite FK (file_id, library_id, media_id).
        tx.execute(
            "UPDATE library_file_units SET library_id = ?2 WHERE file_id = ?1",
            params![id, request.to_library_id.to_string()],
        )?;
        migrate_metadata_inputs(
            &tx,
            &request.from_library_id.to_string(),
            &request.to_library_id.to_string(),
            &request.to_root_id,
            &from_str,
            &to_str,
            &request.sidecars,
        )?;
        migrate_scrape_sidecars(&tx, &id, &request.sidecars)?;
        rewrite_artwork_paths(&tx, &id, &request.sidecars)?;

        let media_id = request.media_id;
        reduce_library_media(&tx, request.from_library_id, media_id)?;
        if request.to_library_id != request.from_library_id {
            reduce_library_media(&tx, request.to_library_id, media_id)?;
        }
        tx.execute(
            "DELETE FROM pending_file_renames WHERE rename_id = ?1",
            params![rename_id],
        )?;
        tx.commit()?;

        // Subscribe facts live outside the library transaction.
        let _ = self.rewrite_fact_paths(media_id, &from_str, &to_str);
        // Queue leftover source-side sidecar bytes (rename failed / copy case).
        for side in &request.sidecars {
            if side.mode == SidecarMode::Move && side.from.is_file() && side.from != side.to {
                let _ = self.enqueue_file_deletion(
                    &side.from.to_string_lossy(),
                    "rename-left-behind-sidecar",
                );
            }
        }
        Ok(())
    }

    /// Drive every unfinished rename: apply disk if needed, then complete.
    pub fn resume_pending_file_renames(&self, now: i64) -> Result<usize, StoreError> {
        let ids = self.list_pending_file_rename_ids()?;
        let mut done = 0usize;
        for id in ids {
            match self.resume_one_file_rename(&id, now) {
                Ok(()) => done += 1,
                Err(error) => {
                    let _ = self.record_file_rename_failure(&id, now, &error.to_string());
                    tracing::error!(rename_id = %id, %error, "pending file rename resume failed");
                }
            }
        }
        Ok(done)
    }

    fn resume_one_file_rename(&self, rename_id: &str, now: i64) -> Result<(), StoreError> {
        let request = self.load_file_rename(rename_id)?;
        if request.phase != "disk_applied" {
            self.apply_file_rename_disk(rename_id, now)?;
        }
        self.complete_file_rename(rename_id, now)
    }

    pub fn list_pending_file_rename_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT rename_id FROM pending_file_renames
             WHERE phase IN ('planned','disk_applied')
             ORDER BY created_at_ms, rename_id",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn load_file_rename(&self, rename_id: &str) -> Result<FileRenameRequest, StoreError> {
        let row: Option<(
            String,
            String,
            String,
            i64,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        )> = self
            .library
            .query_row(
                "SELECT file_id, from_path, to_path, expected_revision, expected_source_version,
                        from_library_id, from_root_id, to_library_id, to_root_id, sidecars_json, phase
                 FROM pending_file_renames WHERE rename_id = ?1",
                params![rename_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            file_id,
            from_path,
            to_path,
            revision,
            source_version,
            from_library_id,
            from_root_id,
            to_library_id,
            to_root_id,
            sidecars_json,
            phase,
        )) = row
        else {
            return Err(StoreError::Missing(format!("rename {rename_id}")));
        };
        let sidecars: Vec<RenameSidecar> = serde_json::from_str(&sidecars_json)
            .map_err(|e| StoreError::Invalid(format!("sidecars json: {e}")))?;
        let file_id: LedgerId = file_id.parse()?;
        let media_id_str: String = self.library.query_row(
            "SELECT media_id FROM library_files WHERE file_id = ?1",
            params![file_id.to_string()],
            |r| r.get(0),
        )?;
        let media_id: MediaId = media_id_str.parse()?;
        Ok(FileRenameRequest {
            rename_id: rename_id.to_string(),
            file_id,
            expected: FileVersion {
                revision,
                source_version,
            },
            from_path: PathBuf::from(from_path),
            to_path: PathBuf::from(to_path),
            from_library_id: from_library_id.parse()?,
            from_root_id,
            to_library_id: to_library_id.parse()?,
            to_root_id,
            sidecars,
            phase,
            media_id,
        })
    }

    fn mark_rename_disk_applied(&self, rename_id: &str, now: i64) -> Result<(), StoreError> {
        let n = self.library.execute(
            "UPDATE pending_file_renames
             SET phase = 'disk_applied', updated_at_ms = ?2, last_error = NULL
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

/// Lock-free disk phase: move the video only. Sidecars move in
/// [`Store::apply_file_rename_disk`] after an execution-time ownership re-check.
pub fn move_rename_bytes(request: &FileRenameRequest) -> Result<(), StoreError> {
    let from = &request.from_path;
    let to = &request.to_path;
    let from_exists = from.is_file();
    let to_exists = to.exists();
    if !from_exists && to_exists {
        return Ok(());
    }
    if !from_exists && !to_exists {
        return Err(StoreError::Missing(format!(
            "rename {}: neither {} nor {} exists on disk",
            request.rename_id,
            from.display(),
            to.display()
        )));
    }
    if from_exists && to_exists {
        return Err(StoreError::Protected(format!(
            "rename {}: both {} and {} exist",
            request.rename_id,
            from.display(),
            to.display()
        )));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)?;
    Ok(())
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
