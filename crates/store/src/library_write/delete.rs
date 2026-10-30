//! File fact removal: `library_files` rows, their coverage, probe bookkeeping and the
//! summaries derived from them, all inside one transaction.
//!
//! When a Media loses its last file it is purged with everything it owns, unless a
//! collection or subscription still references it (see `orphan_media`).

use std::collections::HashSet;

use domain::{LedgerId, LibraryId, MediaId};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::StoreError;
use super::reduce::reduce_library_media;

impl super::super::Store {
    /// Remove `file_ids` from one Library and reduce every summary they produced.
    ///
    /// Returns how many files were actually removed; ids that do not belong to
    /// `library_id` are ignored rather than reported as removed.
    pub fn delete_files(
        &self,
        library_id: LibraryId,
        file_ids: &[LedgerId],
    ) -> Result<usize, StoreError> {
        self.with_reference_write(|library, subscribe| {
            library.delete_files_with_references(subscribe, library_id, file_ids)
        })
    }
}

/// Remove files of one Library inside the caller's transaction, then reduce
/// `library_items`/`library_units`/`library_item_terms` for every affected media.
///
/// Unknown ids, ids of another Library and duplicates are skipped, so callers may
/// pass whatever the management view resolved without re-checking it.
pub(crate) fn delete_files_in_tx(
    tx: &Connection,
    library_id: LibraryId,
    file_ids: &[LedgerId],
) -> Result<(usize, HashSet<MediaId>), StoreError> {
    crate::library_operations::integrity_guard::ensure_integrity_writable(tx, &[library_id])?;
    let library = library_id.to_string();
    let mut affected: HashSet<MediaId> = HashSet::new();
    let mut seen: HashSet<LedgerId> = HashSet::new();
    let mut removed = 0;
    for file_id in file_ids {
        if !seen.insert(*file_id) {
            continue;
        }
        let media_id: Option<String> = tx
            .query_row(
                "SELECT media_id FROM library_files WHERE file_id = ?1 AND library_id = ?2",
                params![file_id.to_string(), library],
                |row| row.get(0),
            )
            .optional()?;
        let Some(media_id) = media_id else {
            continue;
        };
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(
            tx,
            media_id.parse()?,
        )?;
        crate::ledger_cleanup::delete_file_in_tx(tx, &file_id.to_string())?;
        affected.insert(media_id.parse::<MediaId>()?);
        removed += 1;
    }
    for media_id in &affected {
        reduce_library_media(tx, library_id, *media_id)?;
    }
    Ok((removed, affected))
}

#[path = "delete/connection.rs"]
mod connection;
