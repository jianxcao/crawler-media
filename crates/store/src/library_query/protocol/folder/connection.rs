//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn protocol_folder_root_id(&self, root_path: &str) -> Result<Option<String>, StoreError> {
        let root_id = self
            .library
            .query_row(
                "SELECT r.root_id FROM library_roots r
                 WHERE r.display_path = ?1 AND r.enabled = 1
                 ORDER BY r.sort_order, r.library_id
                 LIMIT 1",
                [root_path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(root_id)
    }
}
