//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn begin_library_reconfiguration_prepared(
        &self,
        prepared: &PreparedLibraryReconfiguration,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        let request = &prepared.request;
        let prepared = prepared.roots.clone();
        let library = request.library_id.to_string();
        // Step 3's first item: a pending removal owns the Library *before* it touches
        // `lifecycle_state`, so the removal row — not the lifecycle — is what refuses a
        // second owner. Without this, a Library being deleted is frozen again for a
        // migration that then fights the removal for the same files.
        self.ensure_library_mutable(&library)?;
        let tx = crate::write_tx(self.library)?;
        if let Some(roots) = &prepared {
            self.ensure_removal_keys_available(roots.iter().map(|root| root.path_key.as_str()))?;
        }
        integrity_guard::ensure_no_operation(&tx, request.library_id)?;
        let current: Option<(i64, String)> = tx
            .query_row(
                "SELECT config_revision, lifecycle_state FROM libraries WHERE library_id = ?1",
                params![library],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((revision, lifecycle)) = current else {
            return Err(StoreError::Missing(format!("library {library}")));
        };
        if revision != request.expected_revision {
            return Err(StoreError::Protected(format!(
                "library {library} config revision changed: expected {}, current {revision}",
                request.expected_revision
            )));
        }
        if lifecycle != "ready" {
            return Err(StoreError::Protected(format!(
                "library {library} lifecycle is {lifecycle}"
            )));
        }
        if let Some(prepared) = &prepared
            && roots::has_uncovered_rows(&tx, &library, prepared)?
        {
            return Err(StoreError::Invalid(format!(
                "library {library} has files outside the requested roots; remove them first"
            )));
        }
        let plan = ReconfigurationPlan {
            default_config_revision: None,
            roots: prepared,
            display_locale: request.display_locale.clone(),
        };
        let kind = if plan.roots.is_some() {
            "roots"
        } else {
            "locale"
        };
        tx.execute(
            "INSERT INTO library_operations
                (operation_id, library_id, kind, state, expected_revision, plan_json,
                 cursor_json, error, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, 'frozen', ?4, ?5, NULL, NULL, ?6, ?6)",
            params![
                request.operation_id,
                library,
                kind,
                request.expected_revision,
                serde_json::to_string(&plan)
                    .map_err(|error| StoreError::Invalid(error.to_string()))?,
                now_ms,
            ],
        )?;
        // The freeze barrier: `reindexing` rejects worker writes, and the bumped
        // revision invalidates every in-flight `WriteScope` that pinned the old one.
        tx.execute(
            "UPDATE libraries SET lifecycle_state = 'reindexing', config_revision = ?2
             WHERE library_id = ?1",
            params![library, revision + 1],
        )?;
        tx.commit()?;
        tracing::info!(
            operation_id = %request.operation_id,
            library_id = %request.library_id,
            kind,
            "library reconfiguration began"
        );
        Ok(OperationStatus {
            operation_id: request.operation_id.clone(),
            phase: "frozen".into(),
            state: "frozen".into(),
            error: None,
        })
    }

    pub(super) fn advance_integrity_operation(
        &self,
        tx: crate::write_tx::SqlTransaction<'_>,
        library: &str,
        operation_id: &str,
        plan_json: &str,
        batch: usize,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        let plan: integrity::IntegrityPlan = serde_json::from_str(plan_json)
            .map_err(|e| StoreError::Invalid(format!("integrity plan: {e}")))?;
        match integrity::apply_integrity_batch(&tx, library, &plan, operation_id, batch, now_ms) {
            Ok(status) => {
                tx.commit()?;
                Ok(status)
            }
            Err(error) => {
                tx.rollback()?;
                self.library.execute("UPDATE library_operations SET state='failed',error=?2,updated_at_ms=?3 WHERE operation_id=?1",params![operation_id,error.to_string(),now_ms])?;
                tracing::error!(operation_id,%error,"Integrity batch failed; owner retained");
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_library_batch(
        &self,
        tx: crate::write_tx::SqlTransaction<'_>,
        operation_id: &str,
        library: &str,
        kind: &str,
        batch: usize,
        cursor: &OperationCursor,
        now_ms: i64,
        outcome: Result<roots::BatchOutcome, StoreError>,
    ) -> Result<(), StoreError> {
        let outcome = outcome.and_then(|outcome| {
            if matches!(outcome, roots::BatchOutcome::Done) {
                default_locale::validate(&tx, library)?;
                let valid: bool = tx.query_row("SELECT l.config_revision=o.expected_revision+1 AND l.lifecycle_state='reindexing' FROM libraries l JOIN library_operations o ON o.library_id=l.library_id WHERE o.operation_id=?1 AND l.library_id=?2",params![operation_id,library],|row|row.get(0))?;
                if !valid { return Err(StoreError::Protected("Library rebuild owner revision changed".into())); }
            }
            Ok(outcome)
        });
        match outcome {
            Ok(roots::BatchOutcome::More) => {
                Store::write_operation_progress(&tx, operation_id, "applying", cursor, now_ms)?;
                tracing::info!(
                    operation_id,
                    library_id = %library,
                    kind,
                    batch,
                    "library reconfiguration applying"
                );
                tx.commit()?;
            }
            Ok(roots::BatchOutcome::Done) => {
                tx.execute(
                    "UPDATE libraries SET lifecycle_state = 'ready' WHERE library_id = ?1",
                    params![library],
                )?;
                Store::write_operation_progress(&tx, operation_id, "succeeded", cursor, now_ms)?;
                tracing::info!(
                    operation_id,
                    library_id = %library,
                    kind,
                    batch,
                    "library reconfiguration succeeded"
                );
                tx.commit()?;
            }
            Err(error) => {
                // Nothing of the failed batch is committed: the Library stays frozen on
                // the last applied generation and the operation keeps its cursor, so a
                // retry (or the next startup) resumes instead of exposing a mixed state.
                tx.rollback()?;
                self.library.execute(
                    "UPDATE library_operations
                     SET state = 'failed', error = ?2, updated_at_ms = ?3
                     WHERE operation_id = ?1",
                    params![operation_id, error.to_string(), now_ms],
                )?;
                tracing::error!(
                    operation_id,
                    library_id = %library,
                    %error,
                    "library reconfiguration failed"
                );
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn library_lifecycle_state(&self, library_id: &str) -> Result<String, StoreError> {
        self.library
            .query_row(
                "SELECT lifecycle_state FROM libraries WHERE library_id = ?1",
                params![library_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::Missing(format!("library {library_id}")))
    }

    pub fn unfinished_library_operations(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(&format!(
            "SELECT operation_id FROM library_operations
             WHERE {}
             ORDER BY updated_at_ms, operation_id",
            unfinished_operation_predicate()
        ))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_library_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<OperationStatus>, StoreError> {
        let row: Option<(String, String, Option<String>)> = self
            .library
            .query_row(
                "SELECT state, state, error FROM library_operations WHERE operation_id = ?1",
                params![operation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match row {
            Some((state, phase, error)) => Ok(Some(OperationStatus {
                operation_id: operation_id.to_string(),
                phase,
                state,
                error,
            })),
            None => Ok(None),
        }
    }

    pub fn library_operation_view(
        &self,
        operation_id: &str,
    ) -> Result<Option<LibraryOperationView>, StoreError> {
        let row: Option<(String, String, String, Option<String>, Option<String>, i64)> = self
            .library
            .query_row(
                "SELECT library_id, kind, state, cursor_json, error, updated_at_ms
                 FROM library_operations WHERE operation_id = ?1",
                params![operation_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((library_id, kind, state, cursor, error, updated_at_ms)) = row else {
            return Ok(None);
        };
        Ok(Some(LibraryOperationView {
            operation_id: operation_id.to_string(),
            library_id,
            kind,
            state,
            cursor: decode_operation_cursor(cursor)?,
            error,
            updated_at_ms,
        }))
    }

    pub fn active_library_operation(&self, library_id: &str) -> Result<Option<String>, StoreError> {
        self.library
            .query_row(
                &format!(
                    "SELECT operation_id FROM library_operations
                     WHERE library_id = ?1 AND {}
                     ORDER BY updated_at_ms DESC, operation_id
                     LIMIT 1",
                    unfinished_operation_predicate()
                ),
                params![library_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
}

impl crate::LibraryConnection<'_> {
    pub fn advance_library_operation(
        &self,
        operation_id: &str,
        now_ms: i64,
        max_media: usize,
    ) -> Result<OperationStatus, StoreError> {
        if !(1..=100).contains(&max_media) {
            tracing::warn!(
                operation_id,
                max_media,
                "library operation batch size outside 1..=100 rejected"
            );
            return Err(StoreError::Invalid(format!(
                "max_media {max_media} is outside the interface range 1..=100"
            )));
        }
        let batch = max_media;
        let tx = crate::write_tx(self.library)?;
        let row: Option<(String, String, String, String, i64)> = tx
            .query_row(
                "SELECT library_id, kind, state, plan_json, expected_revision
                 FROM library_operations WHERE operation_id = ?1",
                params![operation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((library, kind, state, plan_json, _expected)) = row else {
            return Err(StoreError::Missing(format!(
                "library operation {operation_id}"
            )));
        };
        // A failed reconfiguration keeps its freeze and is retried from its cursor; failed
        // Integrity execution is retryable from its persisted cursor.
        let retryable = state == FAILED_STATE && RETRYABLE_FAILED_KINDS.contains(&kind.as_str());
        if state == SUCCEEDED_STATE || (state == FAILED_STATE && !retryable) {
            tx.commit()?;
            return Store::read_library_operation(self.library, operation_id);
        }
        if kind == "integrity" {
            return self.advance_integrity_operation(
                tx,
                &library,
                operation_id,
                &plan_json,
                batch,
                now_ms,
            );
        }
        let plan: ReconfigurationPlan = serde_json::from_str(&plan_json)
            .map_err(|error| StoreError::Invalid(format!("operation plan: {error}")))?;
        let mut cursor = Store::read_operation_cursor(&tx, operation_id)?;

        let outcome = match kind.as_str() {
            // A combined roots+locale request is one operation: the locale is applied in the
            // same transaction that completes the last root batch, never dropped.
            "roots" => roots::apply_root_batch(
                &tx,
                &library,
                plan.roots.as_deref().unwrap_or_default(),
                &mut cursor,
                batch,
            )
            .and_then(|outcome| match outcome {
                roots::BatchOutcome::Done => locale::apply_locale(
                    &tx,
                    &library,
                    plan.display_locale.as_deref(),
                    &mut cursor,
                    batch,
                ),
                more => Ok(more),
            }),
            "default_locale" => default_locale::apply(
                &tx,
                &library,
                operation_id,
                plan.default_config_revision,
                &mut cursor,
                batch,
            ),
            "locale" => locale::apply_locale(
                &tx,
                &library,
                plan.display_locale.as_deref(),
                &mut cursor,
                batch,
            ),
            other => Err(StoreError::Invalid(format!(
                "library operation kind {other} is not a reconfiguration"
            ))),
        };

        self.finish_library_batch(
            tx,
            operation_id,
            &library,
            &kind,
            batch,
            &cursor,
            now_ms,
            outcome,
        )?;
        crate::path_key::reload_root_aliases(self.library)?;
        Store::read_library_operation(self.library, operation_id)
    }
}
