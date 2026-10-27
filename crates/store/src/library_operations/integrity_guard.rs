//! A durable Integrity owner can only be issued by validating its frozen revision.
use crate::StoreError;
use domain::{LibraryId, MediaId};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) struct IntegrityOwner {
    operation_id: String,
    library_id: LibraryId,
    revision: i64,
}
impl IntegrityOwner {
    pub(crate) fn acquire(
        tx: &Connection,
        operation_id: &str,
        library_id: LibraryId,
    ) -> Result<Self, StoreError> {
        let revision:i64=tx.query_row("SELECT expected_revision+1 FROM library_operations WHERE operation_id=?1 AND library_id=?2 AND kind='integrity' AND state<>'succeeded'",params![operation_id,library_id.to_string()],|r|r.get(0))?;
        let owner = Self {
            operation_id: operation_id.into(),
            library_id,
            revision,
        };
        owner.validate(tx, library_id)?;
        Ok(owner)
    }
    pub(crate) fn validate(
        &self,
        tx: &Connection,
        library_id: LibraryId,
    ) -> Result<(), StoreError> {
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM library_operations o JOIN libraries l ON l.library_id=o.library_id WHERE o.operation_id=?1 AND o.library_id=?2 AND o.kind='integrity' AND o.state<>'succeeded' AND l.lifecycle_state='reindexing' AND l.config_revision=?3 AND o.expected_revision+1=?3)",params![self.operation_id,library_id.to_string(),self.revision],|r|r.get(0))?;
        if library_id != self.library_id || !valid {
            return Err(StoreError::Protected(
                "Integrity owner or frozen revision changed".into(),
            ));
        }
        Ok(())
    }
}
pub(crate) fn ensure_integrity_writable(
    tx: &Connection,
    libraries: &[LibraryId],
) -> Result<(), StoreError> {
    for library in libraries {
        let blocked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM library_operations WHERE library_id=?1 AND kind='integrity' AND state<>'succeeded')",[library.to_string()],|r|r.get(0))?;
        if blocked {
            tracing::warn!(library_id=%library,"write rejected by Integrity owner");
            return Err(StoreError::Protected(format!(
                "library {library} has an Integrity owner"
            )));
        }
    }
    Ok(())
}
pub(crate) fn ensure_media_integrity_writable(
    tx: &Connection,
    media: MediaId,
) -> Result<(), StoreError> {
    let owner: Option<String> = tx.query_row(
        "SELECT o.library_id FROM library_operations o
         WHERE o.kind='integrity' AND o.state<>'succeeded' AND (
           EXISTS(SELECT 1 FROM library_files f WHERE f.library_id=o.library_id AND f.media_id=?1)
           OR EXISTS(SELECT 1 FROM library_items i WHERE i.library_id=o.library_id AND i.media_id=?1)
           OR EXISTS(SELECT 1 FROM library_metadata_inputs n WHERE n.library_id=o.library_id AND n.media_id=?1))
         LIMIT 1", [media.to_string()], |r| r.get(0)).optional()?;
    if let Some(owner) = owner {
        tracing::warn!(library_id=%owner, media_id=%media, "write rejected by Integrity owner");
        return Err(StoreError::Protected(format!(
            "library {owner} has an Integrity owner"
        )));
    }
    Ok(())
}
pub(crate) fn ensure_no_operation(tx: &Connection, library: LibraryId) -> Result<(), StoreError> {
    let active = super::unfinished_operation_predicate();
    let sql = format!("SELECT EXISTS(SELECT 1 FROM library_operations WHERE library_id=?1 AND ({active})) OR EXISTS(SELECT 1 FROM library_removals WHERE library_id=?1 AND state<>'succeeded') OR EXISTS(SELECT 1 FROM media_operations o JOIN library_files f ON f.media_id IN (o.survivor_id,o.loser_id) WHERE f.library_id=?1 AND o.state<>'succeeded')");
    let blocked: bool = tx.query_row(&sql, [library.to_string()], |r| r.get(0))?;
    if blocked {
        return Err(StoreError::Protected(format!(
            "library {library} already has an operation"
        )));
    }
    Ok(())
}

pub(crate) fn ensure_no_root_reassignment(
    tx: &Connection,
    library: LibraryId,
) -> Result<(), StoreError> {
    let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM root_reassignment_state r WHERE r.state<>'succeeded' AND (r.library_id=?1 OR EXISTS(SELECT 1 FROM library_files f WHERE f.library_id=?1 AND (f.path_key=r.root_key OR substr(f.path_key,1,length(r.root_key)+1)=r.root_key||'/'))))", [library.to_string()], |r| r.get(0))?;
    if pending {
        return Err(StoreError::Protected(format!(
            "library {library} has a root reassignment"
        )));
    }
    Ok(())
}
