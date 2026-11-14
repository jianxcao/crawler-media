//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn ensure_library_query_ready(
        &self,
        library_id: domain::LibraryId,
    ) -> Result<(), crate::StoreError> {
        items::page::ensure_library_query_ready_on(self.library, library_id)
    }
}
