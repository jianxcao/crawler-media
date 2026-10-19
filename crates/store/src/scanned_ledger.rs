use domain::{LedgerId, MediaId};
use rusqlite::params;

use crate::{Store, StoreError};

impl Store {
    /// Rebind one scan-created file fact without merging any Media or history.
    /// Callers must restrict this to ledger IDs inserted by their current scan.
    pub fn rebind_scanned_ledger(
        &self,
        ledger_id: LedgerId,
        media_id: MediaId,
    ) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        tx.execute(
            "UPDATE library_files SET media_id = ?1 WHERE file_id = ?2",
            params![media_id.to_string(), ledger_id.to_string()],
        )?;
        tx.execute(
            "UPDATE library_file_units SET media_id = ?1 WHERE file_id = ?2",
            params![media_id.to_string(), ledger_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
}
