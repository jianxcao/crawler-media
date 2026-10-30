//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn root_reassignments_for_library(
        &self,
        library: &str,
    ) -> Result<Vec<RootReassignment>, StoreError> {
        let mut statement=self.library.prepare(
            "SELECT operation_id,library_id,root_key,config_revision,state,revision,error,updated_at_ms
             FROM root_reassignment_state WHERE library_id=?1 ORDER BY root_key",
        )?;
        let rows = statement.query_map([library], |row| {
            Ok(RootReassignment {
                operation_id: row.get(0)?,
                library_id: row.get(1)?,
                root_key: row.get(2)?,
                config_revision: row.get(3)?,
                state: row.get(4)?,
                revision: row.get(5)?,
                error: row.get(6)?,
                updated_at_ms: row.get(7)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}
