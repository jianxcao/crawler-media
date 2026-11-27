//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::SubscribeConnection<'_> {
    pub fn detach_subscribes_from_library(&self, library: LibraryId) -> Result<usize, StoreError> {
        let tx = crate::write_tx(self.subscribe)?;
        let lib_str = library.to_string();
        tx.execute(
            "INSERT INTO subscribe_library_fences(library_id) VALUES(?1) ON CONFLICT DO NOTHING",
            [&lib_str],
        )?;
        tx.execute(
            "UPDATE subscribe_deferred_candidates
             SET state = 'invalidated', revision = revision + 1, updated_at = ?2
             WHERE state = 'deferred'
               AND subscribe_id IN (SELECT id FROM subscribes WHERE library_id = ?1)",
            rusqlite::params![&lib_str, crate::time::unix_now()],
        )?;
        let detached = tx.execute(
            "UPDATE subscribes SET library_id=NULL, policy_revision = policy_revision + 1 WHERE library_id=?1",
            [lib_str],
        )?;
        tx.commit()?;
        tracing::info!(library_id = %library, detached, "已清除被删除 Library 的 Subscribe 绑定");
        Ok(detached)
    }
}
