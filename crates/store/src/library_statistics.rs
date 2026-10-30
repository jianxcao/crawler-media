//! Planner statistics refresh on the one Library writer.
use crate::{Db, Store, StoreError};
impl Store {
    pub fn optimize_library_statistics(&self) -> Result<(), StoreError> {
        self.with_writer(Db::Library, |conn| {
            conn.execute_batch("PRAGMA optimize=0x10002;")?;
            Ok(())
        })?;
        self.reader_pool(Db::Library).invalidate_planner();
        Ok(())
    }
}
