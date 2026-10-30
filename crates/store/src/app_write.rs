//! Single-connection write transaction seam for the app database.
//!
//! Only `app.db` is covered; catalog/library/subscribe keep their own
//! connections and transactions. The callback must use direct writes on the
//! same connection — never a method that opens its own transaction.

use super::{Store, StoreError};

impl Store {
    /// Run related App writes atomically on their shared writer.
    pub fn app_write<T>(
        &self,
        write: impl FnOnce(&crate::App<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.with_app(|app| app.app_write(write))
    }
}

impl crate::App<'_> {
    pub fn app_write<T>(
        &self,
        write: impl FnOnce(&Self) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            self.app,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let value = match write(self) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(error = %error, "app_write 回滚：回调失败");
                return Err(error);
            }
        };
        tx.commit()?;
        Ok(value)
    }
}
