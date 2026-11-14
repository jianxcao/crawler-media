//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn plan_file_rename_completion(
        &self,
        rename_id: &str,
    ) -> Result<RenameCompletionPlan, StoreError> {
        let request = self.load_file_rename(rename_id)?;
        if !matches!(
            request.phase.as_str(),
            "disk_applied" | "library_published" | "references_applied"
        ) {
            return Err(StoreError::Protected(format!(
                "rename {rename_id} phase {} cannot complete; apply disk first",
                request.phase
            )));
        }
        Ok(RenameCompletionPlan { request })
    }
}
