//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn rebind_scanned_ledger(
        &self,
        ledger_id: LedgerId,
        media_id: MediaId,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
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
