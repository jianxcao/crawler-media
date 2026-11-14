//! Bounded inherited-language rebuild, preserving explicit Library locale.
use super::{OperationCursor, roots::BatchOutcome};
use crate::StoreError;
use rusqlite::{Connection, params};

pub(super) fn apply(
    db: &Connection,
    library: &str,
    operation: &str,
    revision: Option<i64>,
    cursor: &mut OperationCursor,
    batch: usize,
) -> Result<BatchOutcome, StoreError> {
    let valid: bool = db.query_row("SELECT q.revision=?3 AND l.config_revision=o.expected_revision+1 AND l.lifecycle_state='reindexing' FROM library_query_config q,libraries l JOIN library_operations o ON o.library_id=l.library_id WHERE q.id=1 AND l.library_id=?1 AND o.operation_id=?2",params![library,operation,revision],|r|r.get(0))?;
    if !valid {
        return Err(StoreError::Protected(
            "default language rebuild revision changed".into(),
        ));
    }
    let media = super::locale::next_media_batch(
        db,
        library,
        cursor.last_locale_media_id.as_deref(),
        batch,
    )?;
    for id in &media {
        crate::library_write::reduce::reduce_library_media(db, library.parse()?, id.parse()?)?;
    }
    tracing::info!(
        library,
        operation,
        rebuilt = media.len(),
        "default language batch rebuilt"
    );
    cursor.last_locale_media_id = media
        .last()
        .cloned()
        .or_else(|| cursor.last_locale_media_id.clone());
    if media.len() == batch {
        Ok(BatchOutcome::More)
    } else {
        validate(db, library)?;
        Ok(BatchOutcome::Done)
    }
}
pub(super) fn validate(db: &Connection, library: &str) -> Result<(), StoreError> {
    let stale: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM library_items WHERE library_id=?1 AND (query_config_revision<>(SELECT revision FROM library_query_config WHERE id=1) OR query_projection_revision<>item_revision))",[library],|r|r.get(0))?;
    if stale {
        return Err(StoreError::Protected(
            "library projection revision is stale".into(),
        ));
    }
    Ok(())
}
