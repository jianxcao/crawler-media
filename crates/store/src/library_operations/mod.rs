//! Persistent Library reconfiguration operations (`roots` / `locale`).
//!
//! A root or display-locale change is not a single UPDATE: while the new root set is
//! being applied, files, coverage and metadata inputs must never be split across two
//! ownership generations. The operation is therefore persisted in `library_operations`
//! and advanced in bounded phases, so a crash mid-way resumes instead of publishing a
//! mixed-ownership library.
//!
//! Phases: `planned` → `frozen` → `applying` (batched) → `succeeded`, plus `failed`.
//! Any phase after `begin` leaves the Library in `reindexing`, which rejects new
//! worker writes through `ensure_library_writable` until the operation succeeds.

use std::path::PathBuf;

use domain::LibraryId;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::playback::types::OperationStatus;
use crate::{Store, StoreError};

pub mod integrity;
mod locale;
mod roots;

/// A requested root set and/or display locale for one Library.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryReconfiguration {
    pub operation_id: String,
    pub library_id: LibraryId,
    pub expected_revision: i64,
    pub root_paths: Option<Vec<PathBuf>>,
    pub display_locale: Option<String>,
}

/// Persisted plan, decoded from `library_operations.plan_json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ReconfigurationPlan {
    #[serde(default)]
    pub root_paths: Option<Vec<PathBuf>>,
    #[serde(default)]
    pub display_locale: Option<String>,
}

/// Where a batched phase resumes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct OperationCursor {
    #[serde(default)]
    pub last_file_id: Option<String>,
}

/// Read-only view of one persisted operation: its Library, kind, resumable cursor and
/// last update. See [`Store::library_operation_view`].
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryOperationView {
    pub operation_id: String,
    pub library_id: String,
    /// `"roots"` or `"locale"`; see [`LibraryReconfiguration`].
    pub kind: String,
    /// Same value as [`LibraryOperationView::state`]; kept because the HTTP contract
    /// publishes both names.
    pub phase: String,
    pub state: String,
    /// Last applied batch position, or `None` before the first batch.
    pub cursor: Option<serde_json::Value>,
    pub error: Option<String>,
    pub updated_at_ms: i64,
}

fn decode_operation_cursor(raw: Option<String>) -> Result<Option<serde_json::Value>, StoreError> {
    match raw.filter(|raw| !raw.trim().is_empty()) {
        Some(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| StoreError::Invalid(format!("operation cursor: {error}"))),
        None => Ok(None),
    }
}

impl Store {
    /// Persist a reconfiguration intent and freeze the Library for it.
    ///
    /// `expected_revision` is a real CAS on `libraries.config_revision`; a stale
    /// expectation is refused instead of silently reconfiguring a newer layout.
    pub fn begin_library_reconfiguration(
        &self,
        request: &LibraryReconfiguration,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        if request.root_paths.is_none() && request.display_locale.is_none() {
            return Err(StoreError::Invalid(
                "library reconfiguration needs a root set or a locale".into(),
            ));
        }
        if let Some(paths) = &request.root_paths
            && paths.is_empty()
        {
            return Err(StoreError::Invalid(
                "library reconfiguration cannot drop every root".into(),
            ));
        }
        let library = request.library_id.to_string();
        let tx = self.library.unchecked_transaction()?;
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
        let plan = ReconfigurationPlan {
            root_paths: request.root_paths.clone(),
            display_locale: request.display_locale.clone(),
        };
        let kind = if plan.root_paths.is_some() {
            "roots"
        } else {
            "locale"
        };
        tx.execute(
            "INSERT INTO library_operations
                (operation_id, library_id, kind, state, expected_revision, plan_json,
                 cursor_json, error, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, 'planned', ?4, ?5, NULL, NULL, ?6, ?6)",
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
            phase: "planned".into(),
            state: "planned".into(),
            error: None,
        })
    }

    /// Advance one reconfiguration operation by at most `max_media` Media.
    ///
    /// Re-running after a crash continues from the persisted cursor; an operation that
    /// is already terminal returns its stored state without doing work.
    pub fn advance_library_operation(
        &self,
        operation_id: &str,
        now_ms: i64,
        max_media: usize,
    ) -> Result<OperationStatus, StoreError> {
        let batch = max_media.clamp(1, 100);
        let tx = self.library.unchecked_transaction()?;
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
        if state == "succeeded" || state == "failed" {
            tx.commit()?;
            return Self::read_library_operation(&self.library, operation_id);
        }
        let plan: ReconfigurationPlan = serde_json::from_str(&plan_json)
            .map_err(|error| StoreError::Invalid(format!("operation plan: {error}")))?;
        let mut cursor = Self::read_operation_cursor(&tx, operation_id)?;

        let outcome = match kind.as_str() {
            "roots" => roots::apply_root_batch(
                &tx,
                &library,
                plan.root_paths.as_deref().unwrap_or_default(),
                &mut cursor,
                batch,
            ),
            "locale" => locale::apply_locale(&tx, &library, plan.display_locale.as_deref()),
            "integrity" => {
                let integrity_plan: integrity::IntegrityPlan = serde_json::from_str(&plan_json)
                    .map_err(|e| StoreError::Invalid(format!("integrity plan: {e}")))?;
                let res = integrity::apply_integrity_batch(
                    &tx,
                    &library,
                    &integrity_plan,
                    operation_id,
                    batch,
                    now_ms,
                );
                tx.commit()?;
                return res;
            }
            other => Err(StoreError::Invalid(format!(
                "library operation kind {other} is not a reconfiguration"
            ))),
        };

        match outcome {
            Ok(roots::BatchOutcome::More) => {
                Self::write_operation_progress(&tx, operation_id, "applying", &cursor, now_ms)?;
            }
            Ok(roots::BatchOutcome::Done) => {
                tx.execute(
                    "UPDATE libraries SET lifecycle_state = 'ready' WHERE library_id = ?1",
                    params![library],
                )?;
                Self::write_operation_progress(&tx, operation_id, "succeeded", &cursor, now_ms)?;
            }
            Err(error) => {
                // A failed reconfiguration must not leave the Library frozen forever:
                // record the failure and make the Library writable again on the last
                // applied generation.
                tx.execute(
                    "UPDATE libraries SET lifecycle_state = 'ready' WHERE library_id = ?1",
                    params![library],
                )?;
                tx.execute(
                    "UPDATE library_operations
                     SET state = 'failed', error = ?2, updated_at_ms = ?3
                     WHERE operation_id = ?1",
                    params![operation_id, error.to_string(), now_ms],
                )?;
                tx.commit()?;
                tracing::error!(
                    operation_id,
                    library_id = %library,
                    %error,
                    "library reconfiguration failed"
                );
                return Err(error);
            }
        }
        tx.commit()?;
        Self::read_library_operation(&self.library, operation_id)
    }

    /// Current lifecycle state of a Library (`ready`, `reindexing`, ...).
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

    /// Operations left unfinished by a previous process, oldest first.
    ///
    /// Startup recovery advances these before workers accept new Library work.
    pub fn unfinished_library_operations(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT operation_id FROM library_operations
             WHERE state NOT IN ('succeeded', 'failed')
             ORDER BY updated_at_ms, operation_id",
        )?;
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

    /// Read-only projection of one operation, including its resumable cursor.
    ///
    /// [`Store::get_library_operation`] predates the cursor and is already consumed by
    /// three callers, so the richer view is a separate additive read: it never writes.
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
            phase: state.clone(),
            state,
            cursor: decode_operation_cursor(cursor)?,
            error,
            updated_at_ms,
        }))
    }

    /// The operation that currently owns this Library, if any is still running.
    ///
    /// `libraries.lifecycle_state = 'reindexing'` is the freeze verdict; this finds the
    /// row that explains it, so a refused write can hand back its `operation_id`.
    pub fn active_library_operation(&self, library_id: &str) -> Result<Option<String>, StoreError> {
        self.library
            .query_row(
                "SELECT operation_id FROM library_operations
                 WHERE library_id = ?1 AND state NOT IN ('succeeded', 'failed')
                 ORDER BY updated_at_ms DESC, operation_id
                 LIMIT 1",
                params![library_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn read_library_operation(
        conn: &rusqlite::Connection,
        operation_id: &str,
    ) -> Result<OperationStatus, StoreError> {
        conn.query_row(
            "SELECT state, error FROM library_operations WHERE operation_id = ?1",
            params![operation_id],
            |r| {
                let state: String = r.get(0)?;
                let error: Option<String> = r.get(1)?;
                Ok(OperationStatus {
                    operation_id: operation_id.to_string(),
                    phase: state.clone(),
                    state,
                    error,
                })
            },
        )
        .map_err(Into::into)
    }

    fn read_operation_cursor(
        conn: &rusqlite::Connection,
        operation_id: &str,
    ) -> Result<OperationCursor, StoreError> {
        let raw: Option<String> = conn.query_row(
            "SELECT cursor_json FROM library_operations WHERE operation_id = ?1",
            params![operation_id],
            |r| r.get(0),
        )?;
        match raw {
            Some(raw) => serde_json::from_str(&raw)
                .map_err(|error| StoreError::Invalid(format!("operation cursor: {error}"))),
            None => Ok(OperationCursor::default()),
        }
    }

    fn write_operation_progress(
        conn: &rusqlite::Connection,
        operation_id: &str,
        state: &str,
        cursor: &OperationCursor,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        conn.execute(
            "UPDATE library_operations
             SET state = ?2, cursor_json = ?3, updated_at_ms = ?4
             WHERE operation_id = ?1",
            params![
                operation_id,
                state,
                serde_json::to_string(cursor)
                    .map_err(|error| StoreError::Invalid(error.to_string()))?,
                now_ms
            ],
        )?;
        Ok(())
    }
}
