//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::SubscribeConnection<'_> {
    pub fn transfer_user_subscribes(&self, from: UserId, to: UserId) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.subscribe)?;
        tx.execute(
            "UPDATE subscribes SET user_id = ?1 WHERE user_id = ?2",
            params![to.to_string(), from.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

impl crate::LibraryConnection<'_> {
    pub fn clean_user_library_data(
        &self,
        user_id: UserId,
        admin_id: UserId,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        tx.execute(
            "UPDATE collections SET user_id = ?1 WHERE user_id = ?2",
            params![admin_id.to_string(), user_id.to_string()],
        )?;
        for table in [
            "playback_units",
            "playback_sessions",
            "playback_session_ids",
            "playback_logs",
            "playback_metrics",
            "user_library_items",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE user_id = ?1"),
                params![user_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
