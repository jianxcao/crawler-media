//! Scoped file reassignment: move exactly the listed files from one Media to another.
//!
//! Re-identification changes which work a *file* belongs to. It must never rename the
//! whole source Media, and it must never touch a sibling file the caller did not list.
//! The move is one transaction: version check, session termination, ownership migration,
//! and both sides' summaries either all land or none do, so a caller that gets `Err`
//! cannot have been told a file was claimed.

use std::collections::{BTreeSet, HashSet};

use domain::{FileVersion, LedgerId, MediaId};
use rusqlite::{Connection, OptionalExtension, params};

use super::publish::ensure_file_version;
use super::reduce::reduce_library_media;
use crate::{Store, StoreError};

/// An explicit, version-pinned reassignment of files from `from` to `to`.
///
/// `files` is authoritative: a file that is not listed is never moved, and a listed file
/// whose revision moved on since the caller read it is rejected instead of guessed at.
pub struct ReassignFiles {
    /// Exactly the files to move, each pinned to the version the caller saw.
    pub files: Vec<(LedgerId, FileVersion)>,
    /// The Media that currently owns every listed file.
    pub from: MediaId,
    /// The Media that will own them afterwards.
    pub to: MediaId,
    /// CAS guard on the source Media's `metadata_revision`.
    pub expected_media_revision: i64,
}

impl Store {
    /// Move exactly `request.files` from `request.from` to `request.to`.
    ///
    /// Returns the number of files actually moved. The Media anchors themselves — title,
    /// aliases, identity state, metadata revision — are never rewritten here; only file
    /// ownership and the derived summaries on both sides change.
    pub fn reassign_files(
        &self,
        request: &ReassignFiles,
        now_ms: i64,
    ) -> Result<usize, StoreError> {
        validate_request(request)?;
        let tx = self.library.unchecked_transaction()?;
        ensure_target_accepts_reassign(&tx, request.to)?;
        ensure_source_revision(&tx, request.from, request.expected_media_revision)?;
        let target_kind: String = media_kind(&tx, request.to)?;
        let mut libraries = BTreeSet::new();
        for (file_id, version) in &request.files {
            let library = ensure_file_movable(&tx, file_id, version, request.from, &target_kind)?;
            libraries.insert(library);
        }
        let ended = terminate_bound_playback(&tx, &request.files)?;
        for (file_id, _) in &request.files {
            move_file_ownership(&tx, *file_id, request.to)?;
        }
        for library in &libraries {
            let library_id = library.parse::<domain::LibraryId>()?;
            reduce_library_media(&tx, library_id, request.from)?;
            reduce_library_media(&tx, library_id, request.to)?;
        }
        tx.commit()?;
        tracing::info!(
            from = %request.from,
            to = %request.to,
            moved = request.files.len(),
            libraries = libraries.len(),
            sessions_ended = ended,
            now_ms,
            "reassigned explicit files to another Media",
        );
        Ok(request.files.len())
    }
}

fn validate_request(request: &ReassignFiles) -> Result<(), StoreError> {
    if request.from == request.to {
        return Err(StoreError::Invalid(
            "reassign source and target are the same Media".into(),
        ));
    }
    if request.files.is_empty() {
        return Err(StoreError::Invalid(
            "reassign requires at least one explicit file".into(),
        ));
    }
    let mut seen = HashSet::new();
    for (file_id, _) in &request.files {
        if !seen.insert(*file_id) {
            return Err(StoreError::Invalid(format!(
                "reassign lists file {file_id} more than once"
            )));
        }
    }
    Ok(())
}

/// The target must be a live identity: a Media that is merging or retired cannot take files.
///
/// A retired target names the survivor that kept the identity, so a caller can point the
/// operator at `POST /media/merges` instead of silently re-homing files onto another id.
fn ensure_target_accepts_reassign(tx: &Connection, to: MediaId) -> Result<(), StoreError> {
    let row: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT identity_state, survivor_id FROM media WHERE media_id = ?1",
            params![to.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((state, _)) if state != "merging" && state != "retired" => Ok(()),
        Some((_, Some(survivor))) => Err(StoreError::Protected(format!(
            "reassign target {to} is retired; its identity lives on in survivor {survivor}"
        ))),
        Some((state, None)) => Err(StoreError::Protected(format!(
            "reassign target {to} identity is {state}; it cannot take files"
        ))),
        None => Err(StoreError::Missing(format!("reassign target media {to}"))),
    }
}

fn ensure_source_revision(tx: &Connection, from: MediaId, expected: i64) -> Result<(), StoreError> {
    let current: Option<i64> = tx
        .query_row(
            "SELECT metadata_revision FROM media WHERE media_id = ?1",
            params![from.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        return Err(StoreError::Missing(format!("reassign source media {from}")));
    };
    if current != expected {
        return Err(StoreError::Conflict(format!(
            "reassign source {from} revision {current} does not match expected {expected}"
        )));
    }
    Ok(())
}

fn media_kind(tx: &Connection, media_id: MediaId) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT kind FROM media WHERE media_id = ?1",
        params![media_id.to_string()],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| StoreError::Missing(format!("media {media_id}")))
}

/// Verify one listed file is owned by `from`, matches its pinned version, and fits the
/// target kind. Returns the Library the file lives in so the caller can reduce it.
fn ensure_file_movable(
    tx: &Connection,
    file_id: &LedgerId,
    version: &FileVersion,
    from: MediaId,
    target_kind: &str,
) -> Result<String, StoreError> {
    let id = file_id.to_string();
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT media_id, kind, library_id FROM library_files WHERE file_id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((owner, kind, library)) = row else {
        return Err(StoreError::Missing(format!("file {id}")));
    };
    if owner != from.to_string() {
        return Err(StoreError::Conflict(format!(
            "file {id} is owned by Media {owner}, not {from}"
        )));
    }
    if kind != target_kind {
        return Err(StoreError::Conflict(format!(
            "file {id} has kind {kind}, which the target Media does not accept (kind {target_kind})"
        )));
    }
    ensure_file_version(tx, &id, version)?;
    Ok(library)
}

/// End live playback bound to files that are changing owner. The old Media's resume package
/// (position, duration, source binding, tracks) is a user fact and stays untouched: an
/// unrecoverable position is not a deleted one. Source selection reports the binding as
/// unavailable once the file no longer covers that Media; the new Media inherits nothing.
/// Session *history* is left intact so the original binding stays auditable.
fn terminate_bound_playback(
    tx: &Connection,
    files: &[(LedgerId, FileVersion)],
) -> Result<usize, StoreError> {
    let mut ended = 0usize;
    for (file_id, _) in files {
        let id = file_id.to_string();
        ended += tx.execute(
            "UPDATE playback_session_ids SET state = 'closed'
             WHERE file_id = ?1 AND state <> 'closed'",
            params![id],
        )?;
        ended += tx.execute(
            "DELETE FROM playback_sessions WHERE file_id = ?1",
            params![id],
        )?;
    }
    Ok(ended)
}

/// Point the file row and its unit coverage at the new Media. `library_id` never changes,
/// so the Library-level foreign keys stay satisfied.
fn move_file_ownership(tx: &Connection, file_id: LedgerId, to: MediaId) -> Result<(), StoreError> {
    let id = file_id.to_string();
    let target = to.to_string();
    tx.execute(
        "UPDATE library_files SET media_id = ?2 WHERE file_id = ?1",
        params![id, target],
    )?;
    tx.execute(
        "UPDATE library_file_units SET media_id = ?2 WHERE file_id = ?1",
        params![id, target],
    )?;
    Ok(())
}
