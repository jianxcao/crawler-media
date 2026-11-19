//! Correct a failed roots plan only while its first batch has not committed.
use rusqlite::{OptionalExtension, params};

use super::{
    LibraryOperationView, ReconfigurationPlan, root_conflicts, root_keys::PreparedRoot, roots,
};
use crate::{PreparedLibraryRoots, Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum RootOperationRetryError {
    #[error("operation does not own the requested frozen revision")]
    NotOwner,
    #[error("root operation has committed batches")]
    NotReplannable,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl Store {
    pub fn replace_failed_roots_operation(
        &self,
        operation_id: &str,
        expected_revision: i64,
        roots: &PreparedLibraryRoots,
        now_ms: i64,
    ) -> Result<LibraryOperationView, RootOperationRetryError> {
        self.library_write_with_error(RootOperationRetryError::Store, |db| {
            db.replace_failed_roots_operation(operation_id, expected_revision, roots, now_ms)
        })
    }
}

impl crate::LibraryConnection<'_> {
    fn replace_failed_roots_operation(
        &self,
        operation_id: &str,
        expected_revision: i64,
        prepared: &PreparedLibraryRoots,
        now_ms: i64,
    ) -> Result<LibraryOperationView, RootOperationRetryError> {
        use RootOperationRetryError::{NotOwner, NotReplannable};
        let tx = self.library;
        let row: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT o.library_id,o.plan_json,o.cursor_json
             FROM library_operations o JOIN libraries l ON l.library_id=o.library_id
             WHERE o.operation_id=?1 AND o.kind='roots' AND o.state='failed'
               AND l.lifecycle_state='reindexing' AND l.config_revision=?2
               AND o.expected_revision+1=l.config_revision",
                params![operation_id, expected_revision],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::from)?;
        let Some((library, json, cursor)) = row else {
            return Err(NotOwner);
        };
        self.ensure_library_mutable(&library)?;
        if self.active_library_operation(&library)?.as_deref() != Some(operation_id) {
            return Err(NotOwner);
        }
        if cursor.is_some() {
            return Err(NotReplannable);
        }
        if prepared.entries.is_empty() {
            return Err(StoreError::Invalid(
                "library reconfiguration cannot drop every root".into(),
            )
            .into());
        }
        self.ensure_removal_keys_available(prepared.entries.iter().map(|(_, key)| key.as_str()))?;
        root_conflicts::ensure_root_keys_available(
            tx,
            &library,
            prepared.entries.iter().map(|(_, key)| key.as_str()),
        )?;
        let requested = prepared
            .entries
            .iter()
            .map(|(display, key)| PreparedRoot {
                display_path: display.into(),
                path_key: key.clone(),
            })
            .collect::<Vec<_>>();
        if roots::has_uncovered_rows(tx, &library, &requested)? {
            return Err(StoreError::Invalid(format!(
                "library {library} has files outside the requested roots; remove them first"
            ))
            .into());
        }
        let mut plan: ReconfigurationPlan = serde_json::from_str(&json).map_err(|error| {
            StoreError::Invalid(format!("root operation {operation_id} plan: {error}"))
        })?;
        plan.roots = Some(requested);
        let json = serde_json::to_string(&plan).map_err(StoreError::from)?;
        tx.execute(
            "UPDATE library_operations SET plan_json=?2,updated_at_ms=?3 WHERE operation_id=?1",
            params![operation_id, json, now_ms],
        )
        .map_err(StoreError::from)?;
        let view = self.library_operation_view(operation_id)?.ok_or(NotOwner)?;
        tracing::info!(
            operation_id,
            library_id = library,
            "failed Library root plan corrected"
        );
        Ok(view)
    }
}
