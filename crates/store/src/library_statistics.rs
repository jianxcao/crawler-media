//! Planner statistics maintenance on the existing Library writer connection.

use crate::{Store, StoreError};

impl Store {
    /// Refresh useful planner statistics after a completed scan round.
    ///
    /// Match the all-tables optimize hint used when opening a writer, without
    /// creating another connection or changing its SQLite tuning policy.
    pub fn optimize_library_statistics(&self) -> Result<(), StoreError> {
        if self.read_only {
            return Err(StoreError::Protected(
                "Library statistics require the writer Store".into(),
            ));
        }
        self.library.execute_batch("PRAGMA optimize=0x10002;")?;
        Ok(())
    }
}
