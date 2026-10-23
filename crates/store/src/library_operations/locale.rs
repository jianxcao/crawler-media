//! Display-locale change for a reconfiguration operation.
//!
//! The switch persists the new display locale, then recomposes every Media card of the
//! Library in keyset batches (`last_locale_media_id`), so the operation only succeeds once
//! no card still shows the old language. Already adopted and already written NFO inputs
//! keep their imported language: no `library_metadata_inputs.locale` row is rewritten.

use rusqlite::{Connection, params};

use super::OperationCursor;
use super::roots::BatchOutcome;
use crate::StoreError;
use crate::library_write::reduce::reduce_library_media;

pub(crate) fn apply_locale(
    tx: &Connection,
    library: &str,
    locale: Option<&str>,
    cursor: &mut OperationCursor,
    batch: usize,
) -> Result<BatchOutcome, StoreError> {
    let Some(locale) = locale else {
        return Ok(BatchOutcome::Done);
    };
    let trimmed = locale.trim();
    if trimmed.is_empty() {
        return Err(StoreError::Invalid("display locale cannot be empty".into()));
    }
    // Idempotent on every batch: a resumed operation re-asserts the same target.
    tx.execute(
        "UPDATE libraries SET display_locale = ?2 WHERE library_id = ?1",
        params![library, trimmed],
    )?;
    let media = next_media_batch(tx, library, cursor.last_locale_media_id.as_deref(), batch)?;
    let library_id = library.parse()?;
    for media_id in &media {
        reduce_library_media(tx, library_id, media_id.parse()?)?;
    }
    tracing::info!(
        library_id = library,
        locale = trimmed,
        rebuilt = media.len(),
        after = cursor.last_locale_media_id.as_deref().unwrap_or(""),
        "library display locale batch rebuilt"
    );
    if media.len() < batch {
        return Ok(BatchOutcome::Done);
    }
    cursor.last_locale_media_id = media.last().cloned();
    Ok(BatchOutcome::More)
}

/// Media with published files in this Library after the keyset, in id order.
fn next_media_batch(
    tx: &Connection,
    library: &str,
    after: Option<&str>,
    batch: usize,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = tx.prepare_cached(
        "SELECT DISTINCT media_id FROM library_files
         WHERE library_id = ?1 AND media_id > ?2
         ORDER BY media_id LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![library, after.unwrap_or(""), batch as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
