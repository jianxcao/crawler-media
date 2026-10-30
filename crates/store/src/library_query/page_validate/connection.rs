//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn validate_page_snapshot(
        &self,
        snapshot: &PageSnapshot,
    ) -> Result<PageValidation, StoreError> {
        let tx = crate::read_tx(self.library)?;
        let result = validate_on(&tx, snapshot)?;
        tx.commit()?;
        Ok(result)
    }
}
