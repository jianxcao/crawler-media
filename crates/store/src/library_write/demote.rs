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
        let tx = crate::write_tx(&self.library)?;
        let mut seen = HashSet::new();
        let mut grouped: HashMap<LibraryId, Vec<LedgerId>> = HashMap::new();
        for request in files {
            let file = &request.file;
            if !seen.insert(file.file_id) || request.scope.library_id != file.library_id {
                return Err(StoreError::Invalid(
                    "duplicate file or mismatched demotion scope".into(),
                ));
            }
            ensure_library_writable(&tx, &file.library_id.to_string())?;
            ensure_config_revision(&tx, &request.scope)?;
            ensure_media_writable(&tx, file.media_id)?;
            super::publish_checked::validate_context(&tx, file, Some(file))?;
            ensure_no_placement_or_rename(&tx, file)?;
            let key = crate::path_key::canonical_recorded_key(&tx, &file.path.to_string_lossy());
            crate::pending_file_deletions::ensure_path_not_pending_deletion(&tx, &key)?;
            // Keep the file's recorded owner/root, rather than inferring a new owner from disk.
            tx.execute("INSERT INTO unidentified(library_id,root_id,path,path_key,confidence,unidentified_id)
                VALUES(?1,?2,?3,?4,?5,?6)", params![file.library_id.to_string(), file.root_id,
                file.path.to_string_lossy(), key, file.confidence.as_str(), uuid::Uuid::new_v4().to_string()])?;
            tx.execute("UPDATE pending_root_reassignments SET unidentified_cursor_key='',unidentified_cursor_id=''
                WHERE (root_key='/' OR ?1=root_key OR substr(?1,1,length(root_key)+1)=root_key||'/')
                AND unidentified_cursor_key>=?1", [&key])?;
            tx.execute("UPDATE root_reassignment_state SET state='queued',revision=revision+1,error=NULL,
                updated_at_ms=?1 WHERE EXISTS(SELECT 1 FROM pending_root_reassignments p
                WHERE p.kind=root_reassignment_state.kind AND p.root_key=root_reassignment_state.root_key)
                AND (root_key='/' OR ?2=root_key OR substr(?2,1,length(root_key)+1)=root_key||'/')",
                params![crate::unix_now()*1000, key])?;
            grouped
                .entry(file.library_id)
                .or_default()
                .push(file.file_id);
        }
        for (library, ids) in grouped {
            delete_files_in_tx(&tx, library, &ids)?;
        }
        tx.commit()?;
        tracing::info!(
            files = files.len(),
            "Published files demoted to Unidentified"
        );
        Ok(files.len())
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
