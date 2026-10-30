//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn library_roster_stats(&self, library_id: &str) -> Result<LibraryRosterStats, StoreError> {
        let (item_count, total_size): (i64, Option<i64>) = self.library.query_row(
            "SELECT COUNT(*), SUM(size_bytes) FROM library_items WHERE library_id = ?1",
            params![library_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        let (file_count, strm_file_count): (i64, i64) = self.library.query_row(
            "SELECT COUNT(*),
                    COUNT(CASE WHEN path_key LIKE '%.strm' THEN 1 END)
             FROM library_files
             WHERE library_id = ?1 AND presence_state = 'present'
               AND publication_state = 'published'",
            params![library_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        Ok(LibraryRosterStats {
            item_count: item_count as u64,
            file_count: file_count as u64,
            total_size_bytes: total_size.unwrap_or(0) as u64,
            strm_file_count: strm_file_count as u64,
        })
    }
}
