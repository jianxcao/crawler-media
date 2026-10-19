use domain::{LedgerId, MediaId};
use rusqlite::{OptionalExtension, params};

use super::library_write::{ensure_media_writable, reduce_library_media};
use crate::{Store, StoreError};

impl Store {
    /// Rebind one scan-created file fact to another Media inside a single transaction.
    ///
    /// Only the file, its coverage and the two affected summaries change: the previous
    /// Media keeps its own global user facts, collections and subscriptions, and the new
    /// Media gains the file. Callers must restrict this to files their current scan
    /// created; identity merging is a different operation.
    pub fn rebind_scanned_ledger(
        &self,
        ledger_id: LedgerId,
        media_id: MediaId,
    ) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let file_id = ledger_id.to_string();
        let current: Option<(String, String, String)> = tx
            .query_row(
                "SELECT library_id, media_id, kind FROM library_files WHERE file_id = ?1",
                params![file_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((library_id, previous_media, kind)) = current else {
            return Err(StoreError::Missing(format!("file {ledger_id}")));
        };
        ensure_media_writable(&tx, media_id)?;
        ensure_kind_matches(&tx, media_id, &kind)?;
        if previous_media == media_id.to_string() {
            return Ok(());
        }

        tx.execute(
            "UPDATE library_files SET media_id = ?1 WHERE file_id = ?2",
            params![media_id.to_string(), file_id],
        )?;
        tx.execute(
            "UPDATE library_file_units SET media_id = ?1 WHERE file_id = ?2",
            params![media_id.to_string(), file_id],
        )?;

        // Both summaries are rebuilt before commit; the deferred composite foreign keys
        // (preferred_file_id) are only satisfied once this finished.
        let library: domain::LibraryId = library_id.parse()?;
        let previous: MediaId = previous_media.parse()?;
        reduce_library_media(&tx, library, previous)?;
        reduce_library_media(&tx, library, media_id)?;
        tx.commit()?;
        Ok(())
    }
}

fn ensure_kind_matches(
    tx: &rusqlite::Connection,
    media_id: MediaId,
    kind: &str,
) -> Result<(), StoreError> {
    let media_kind: Option<String> = tx
        .query_row(
            "SELECT kind FROM media WHERE media_id = ?1",
            params![media_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let media_kind = media_kind.ok_or_else(|| StoreError::Missing(format!("media {media_id}")))?;
    if media_kind != kind {
        return Err(StoreError::Protected(format!(
            "media {media_id} is {media_kind} but the file is {kind}"
        )));
    }
    Ok(())
}
