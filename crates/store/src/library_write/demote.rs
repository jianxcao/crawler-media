//! Park exactly the observed file identities and retire their published facts atomically.
use super::{
    WriteScope,
    delete::delete_files_in_tx,
    publish::{ensure_config_revision, ensure_library_writable, ensure_media_writable},
};
use crate::{Store, StoreError};
use domain::{LedgerId, LibraryFile, LibraryId};
use rusqlite::params;
use std::collections::{HashMap, HashSet};

pub struct FileDemotion {
    pub scope: WriteScope,
    pub file: LibraryFile,
}

impl Store {
    /// No bytes move. A stale file, changed Library or failed cleanup aborts the whole batch.
    pub fn demote_files_to_unidentified(
        &self,
        files: &[FileDemotion],
    ) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.demote_files_to_unidentified(files))
    }
}

/// Placement and rename owners retain their file until their own operation completes.
fn ensure_no_placement_or_rename(
    tx: &rusqlite::Connection,
    file: &LibraryFile,
) -> Result<(), StoreError> {
    let (published, renaming): (bool, bool) = tx.query_row(
        "SELECT publication_state='published', EXISTS(SELECT 1 FROM pending_file_renames r
            WHERE r.file_id=f.file_id) FROM library_files f WHERE f.file_id=?1",
        [file.file_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if file.publication != domain::FilePublication::Published || !published || renaming {
        tracing::warn!(file_id=%file.file_id, published, renaming, "File demotion rejected while placement or rename owns the file");
        return Err(StoreError::Protected(format!(
            "file {} has a placement or rename owner",
            file.file_id
        )));
    }
    Ok(())
}

#[path = "demote/connection.rs"]
mod connection;
