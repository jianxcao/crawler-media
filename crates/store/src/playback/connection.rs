//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn playback_write<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        if !self.library.is_autocommit() {
            return operation(self);
        }
        let transaction = crate::write_tx(self.library)?;
        let result = operation(self)?;
        transaction.commit()?;
        Ok(result)
    }
}
