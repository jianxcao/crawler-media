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
        self.with_library_write(|db| db.rebind_scanned_ledger(ledger_id, media_id))
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

#[path = "scanned_ledger/connection.rs"]
mod connection;
