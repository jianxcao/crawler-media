//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn begin_library_integrity(
        &self,
        library: LibraryId,
        repair: bool,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let lib_str = library.to_string();

        let current: Option<(i64, String)> = tx
            .query_row(
                "SELECT config_revision, lifecycle_state FROM libraries WHERE library_id = ?1",
                params![lib_str],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((revision, lifecycle)) = current else {
            return Err(StoreError::Missing(format!("library {library}")));
        };
        super::super::integrity_guard::ensure_no_operation(&tx, library)?;
        super::super::integrity_guard::ensure_no_shared_integrity(&tx, library)?;
        super::super::integrity_guard::ensure_no_root_reassignment(&tx, library)?;
        if lifecycle != "ready" {
            return Err(StoreError::Protected(format!(
                "library {library} lifecycle is {lifecycle}"
            )));
        }

        let plan = IntegrityPlan { repair };
        let plan_json =
            serde_json::to_string(&plan).map_err(|e| StoreError::Invalid(e.to_string()))?;

        tx.execute(
            "INSERT INTO library_operations
                (operation_id, library_id, kind, state, expected_revision, plan_json,
                 cursor_json, error, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'integrity', 'planned', ?3, ?4, NULL, NULL, ?5, ?5)",
            params![operation_id, lib_str, revision, plan_json, now_ms],
        )?;

        tx.execute("UPDATE libraries SET config_revision=config_revision+1,lifecycle_state='reindexing' WHERE library_id=?1",[&lib_str])?;
        tx.commit()?;
        tracing::info!(library_id=%library,operation_id,repair,"Integrity owner acquired");
        Ok(OperationStatus {
            operation_id: operation_id.to_string(),
            phase: "planned".into(),
            state: "planned".into(),
            error: None,
        })
    }

    pub fn library_integrity_report(
        &self,
        operation_id: &str,
    ) -> Result<IntegrityReport, StoreError> {
        let cursor_json: Option<Option<String>> = self
            .library
            .query_row(
                "SELECT cursor_json FROM library_operations WHERE operation_id = ?1",
                params![operation_id],
                |r| r.get(0),
            )
            .optional()?;

        let Some(Some(raw)) = cursor_json else {
            return Ok(IntegrityReport::default());
        };
        if raw.trim().is_empty() {
            return Ok(IntegrityReport::default());
        }

        let state: IntegrityCursor = serde_json::from_str(&raw)
            .map_err(|e| StoreError::Invalid(format!("integrity cursor: {e}")))?;
        Ok(state.report)
    }
}
