//! Clear Subscribe targets after their Library finalization has committed.

use domain::LibraryId;

use crate::{Store, StoreError};

impl Store {
    /// Leave Subscribes using the same-kind default after their target is removed.
    /// Call after committing library.db; this transaction owns only subscribe.db.
    pub fn detach_subscribes_from_library(&self, library: LibraryId) -> Result<usize, StoreError> {
        let tx = crate::write_tx(&self.subscribe)?;
        let detached = tx.execute(
            "UPDATE subscribes SET library_id=NULL WHERE library_id=?1",
            [library.to_string()],
        )?;
        tx.commit()?;
        tracing::info!(library_id = %library, detached, "已清除被删除 Library 的 Subscribe 绑定");
        Ok(detached)
    }
}
