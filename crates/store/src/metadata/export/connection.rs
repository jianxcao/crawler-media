//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn begin_nfo_export(
        &self,
        request: &NfoExportRequest,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let library = request.library_id.to_string();
        let lifecycle: Option<String> = tx
            .query_row(
                "SELECT lifecycle_state FROM libraries WHERE library_id = ?1",
                params![library],
                |row| row.get(0),
            )
            .optional()?;
        let Some(lifecycle) = lifecycle else {
            return Err(StoreError::Missing(format!("library {library}")));
        };
        if lifecycle != "ready" && lifecycle != "reindexing" {
            return Err(StoreError::Protected(format!(
                "library {library} lifecycle is {lifecycle}"
            )));
        }

        ensure_export_binding(&tx, request)?;
        let plan_json = encode_plan(request)?;
        let key = crate::path_key::canonical_recorded_key(&tx, &request.path.to_string_lossy());
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT state, plan_json FROM library_operations WHERE operation_id=?1",
                [&request.operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((state, existing_plan)) = existing {
            if state != "succeeded" && state != "failed" && existing_plan == plan_json {
                return Ok(OperationStatus {
                    operation_id: request.operation_id.clone(),
                    phase: state.clone(),
                    state,
                    error: None,
                });
            }
            return Err(StoreError::Conflict(format!(
                "nfo export {} already has a different or terminal intent",
                request.operation_id
            )));
        }
        let owner: Option<String> = tx.query_row(
            "SELECT operation_id FROM library_operations WHERE resource_key=?1 AND kind='scrape' AND state NOT IN ('succeeded','failed')",
            [&key], |row| row.get(0),
        ).optional()?;
        if let Some(owner) = owner {
            tracing::warn!(path=%key, owner=%owner, requested=%request.operation_id, "NFO export path already owned");
            return Err(StoreError::Conflict(format!(
                "nfo path {key} owned by {owner}"
            )));
        }
        tx.execute(
            "INSERT INTO library_operations
                (operation_id, library_id, kind, state, expected_revision, plan_json,
                 cursor_json, error, created_at_ms, updated_at_ms, resource_key)
             VALUES (?1, ?2, ?3, 'planned', ?4, ?5, NULL, NULL, ?6, ?6, ?7)",
            params![
                request.operation_id,
                library,
                NFO_EXPORT_KIND,
                request.config_revision,
                plan_json,
                now_ms,
                key
            ],
        )?;
        tx.commit()?;
        Ok(OperationStatus {
            operation_id: request.operation_id.clone(),
            phase: "planned".into(),
            state: "planned".into(),
            error: None,
        })
    }

    pub(crate) fn replan_nfo_export(
        &self,
        request: &NfoExportRequest,
        now_ms: i64,
    ) -> Result<NfoExportRequest, StoreError> {
        let tx = crate::write_tx(self.library)?;
        ensure_export_binding(&tx, request)?;
        let key = crate::path_key::canonical_recorded_key(&tx, &request.path.to_string_lossy());
        let current: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT state, resource_key FROM library_operations
                 WHERE operation_id = ?1 AND kind = ?2",
                params![request.operation_id, NFO_EXPORT_KIND],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, owner_key)) = current else {
            return Err(StoreError::Missing(format!(
                "nfo export operation {}",
                request.operation_id
            )));
        };
        if state == "succeeded" || state == "failed" {
            return Err(StoreError::Conflict(format!(
                "nfo export {} is {state} and cannot be re-planned",
                request.operation_id
            )));
        }
        if owner_key.as_deref() != Some(key.as_str()) {
            return Err(StoreError::Conflict(format!(
                "nfo export {} owns {:?}, not {key}",
                request.operation_id, owner_key
            )));
        }
        let plan_json = encode_plan(request)?;
        tx.execute(
            "UPDATE library_operations
             SET plan_json = ?2, expected_revision = ?3, updated_at_ms = ?4
             WHERE operation_id = ?1",
            params![
                request.operation_id,
                plan_json,
                request.config_revision,
                now_ms
            ],
        )?;
        tx.commit()?;
        tracing::info!(
            operation_id = %request.operation_id,
            export_id = %request.export_id,
            path = %key,
            "NFO 导出意图已按新的有效元数据重写"
        );
        Ok(request.clone())
    }

    pub fn nfo_export_pending(&self, operation_id: &str) -> Result<bool, StoreError> {
        let state: Option<String> = self
            .library
            .query_row(
                "SELECT state FROM library_operations WHERE operation_id = ?1 AND kind = ?2",
                params![operation_id, NFO_EXPORT_KIND],
                |row| row.get(0),
            )
            .optional()?;
        Ok(state.is_some_and(|state| state != "succeeded" && state != "failed"))
    }

    pub(crate) fn nfo_export_plan_current(
        &self,
        request: &NfoExportRequest,
    ) -> Result<bool, StoreError> {
        let key =
            crate::path_key::canonical_recorded_key(self.library, &request.path.to_string_lossy());
        let plan = encode_plan(request)?;
        Ok(self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_operations WHERE operation_id = ?1 AND kind = ?2 AND resource_key = ?3 AND plan_json = ?4 AND state NOT IN ('succeeded', 'failed'))",
            params![request.operation_id, NFO_EXPORT_KIND, key, plan], |row| row.get(0),
        )?)
    }

    pub fn nfo_export_binding_current(&self, request: &NfoExportRequest) -> Result<(), StoreError> {
        ensure_export_binding(self.library, request)
    }

    pub fn fail_nfo_export(
        &self,
        operation_id: &str,
        error: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let key: Option<String> = self
            .library
            .query_row(
                "SELECT resource_key FROM library_operations WHERE operation_id = ?1 AND kind = ?2",
                params![operation_id, NFO_EXPORT_KIND],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let Some(key) = key else {
            return Ok(());
        };
        super::super::export_lease::while_inactive(self.data_dir, &key, || {
            self.library.execute(
                "UPDATE library_operations SET state = 'failed', error = ?2, updated_at_ms = ?3
             WHERE operation_id = ?1 AND kind = ?4 AND state NOT IN ('succeeded', 'failed')",
                params![operation_id, error, now_ms, NFO_EXPORT_KIND],
            )?;
            Ok(())
        })
    }

    pub(crate) fn fail_owned_nfo_export(
        &self,
        request: &NfoExportRequest,
        error: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let plan = encode_plan(request)?;
        let key =
            crate::path_key::canonical_recorded_key(self.library, &request.path.to_string_lossy());
        self.library.execute(
            "UPDATE library_operations SET state = 'failed', error = ?2, updated_at_ms = ?3
             WHERE operation_id = ?1 AND kind = ?4 AND resource_key = ?5
               AND plan_json = ?6 AND state NOT IN ('succeeded', 'failed')",
            params![
                request.operation_id,
                error,
                now_ms,
                NFO_EXPORT_KIND,
                key,
                plan
            ],
        )?;
        Ok(())
    }

    pub fn nfo_export_registration(
        &self,
        operation_id: &str,
    ) -> Result<Option<NfoExportRegistration>, StoreError> {
        let row: Option<(String, String)> = self
            .library
            .query_row(
                "SELECT state, plan_json FROM library_operations
                 WHERE operation_id = ?1 AND kind = ?2",
                params![operation_id, NFO_EXPORT_KIND],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, plan_json)) = row else {
            return Ok(None);
        };
        if state != "succeeded" {
            return Ok(None);
        }
        Ok(Some(registration(
            operation_id,
            &state,
            decode_plan(&plan_json)?,
        )))
    }

    pub fn pending_nfo_exports(&self) -> Result<Vec<NfoExportRequest>, StoreError> {
        let tx = crate::read_tx(self.library)?;
        let mut requests = Vec::new();
        for (_, plan_json) in scrape_operations(&tx)? {
            requests.push(decode_plan(&plan_json)?);
        }
        Ok(requests)
    }
}
