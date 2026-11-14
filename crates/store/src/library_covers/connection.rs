//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn library_cover_revision(&self, id: &str) -> Result<Option<i64>, StoreError> {
        self.library
            .query_row(
                "SELECT COALESCE(cover_revision, 0) FROM libraries WHERE library_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
}
