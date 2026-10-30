//! Persistent Library reconfiguration operations (`roots` / `locale`).
//!
//! A root or display-locale change is not a single UPDATE: while the new root set is
//! being applied, files, coverage and metadata inputs must never be split across two
//! ownership generations. The operation is therefore persisted in `library_operations`
//! and advanced in bounded phases, so a crash mid-way resumes instead of publishing a
//! mixed-ownership library.
//!
//! Phases: `frozen` → `applying` (batched) → `succeeded`, plus `failed`. The `frozen` row
//! is written in the same transaction as the freeze itself, so the barrier is durable from
//! the moment `begin` returns; Integrity also owns a durable freeze from its planned state.
//! Any phase after `begin` leaves the Library in `reindexing`, which rejects new
//! worker writes through `ensure_library_writable` until the operation succeeds.

use std::path::PathBuf;

use domain::LibraryId;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::playback::types::OperationStatus;
use crate::{Store, StoreError};

/// `library_operations.state` values the rest of this module distinguishes.
pub(crate) const SUCCEEDED_STATE: &str = "succeeded";
pub(crate) const FAILED_STATE: &str = "failed";

/// Kinds whose `failed` row is a *retryable* failure: the next attempt resumes from that
/// row, which still carries the Library freeze and the cursor. Dropping it would strand the
/// Library in `reindexing` with nothing left to resume.
const RETRYABLE_FAILED_KINDS: [&str; 4] = ["roots", "locale", "integrity", "default_locale"];

fn retryable_failed_kinds_sql() -> String {
    RETRYABLE_FAILED_KINDS
        .iter()
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Predicate for a row that has *not* reached its end (startup recovery, active-op read).
pub(crate) fn unfinished_operation_predicate() -> String {
    format!(
        "state <> 'succeeded' AND (state <> 'failed' OR kind IN ({}))",
        retryable_failed_kinds_sql()
    )
}

/// Predicate for a row that has reached its end, so retention may drop it.
///
/// The complement of [`unfinished_operation_predicate`] by construction: one list decides
/// both, so retention can never drop a row that recovery would still resume.
pub(crate) fn terminal_operation_predicate() -> String {
    format!(
        "state = 'succeeded' OR (state = 'failed' AND kind NOT IN ({}))",
        retryable_failed_kinds_sql()
    )
}

mod default_locale;
pub mod integrity;
pub(crate) mod integrity_guard;
mod integrity_query;
mod locale;
mod preparation;
mod root_keys;
pub use preparation::PreparedLibraryReconfiguration;
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
    pub default_config_revision: Option<i64>,
    pub roots: Option<Vec<root_keys::PreparedRoot>>,
    pub display_locale: Option<String>,
}

/// Where a batched phase resumes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct OperationCursor {
    #[serde(default)]
    pub last_file_id: Option<String>,
    /// Set once every file moved: unidentified rows and metadata inputs that no file
    /// path covers are re-parented in their own keyset pass.
    #[serde(default)]
    pub files_done: bool,
    #[serde(default)]
    pub last_unidentified_path: Option<String>,
    #[serde(default)]
    pub unidentified_done: bool,
    #[serde(default)]
    pub last_input_path: Option<String>,
    /// Locale rebuild keyset: the last Media whose card was recomposed.
    #[serde(default)]
    pub last_locale_media_id: Option<String>,
}

/// Read-only view of one persisted operation: its Library, kind, resumable cursor and
/// last update. See [`Store::library_operation_view`].
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryOperationView {
    pub operation_id: String,
    pub library_id: String,
    /// `"roots"` or `"locale"`; see [`LibraryReconfiguration`].
    pub kind: String,
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
        let prepared = request.prepare()?;
        self.begin_library_reconfiguration_prepared(&prepared, now_ms)
    }

    /// Commit an intent whose filesystem observations were made without Store.
    pub fn begin_library_reconfiguration_prepared(
        &self,
        prepared: &PreparedLibraryReconfiguration,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        self.with_library_write(|db| db.begin_library_reconfiguration_prepared(prepared, now_ms))
    }

    /// Advance one reconfiguration operation by at most `max_media` Media.
    ///
    /// `max_media` is an interface contract, not a hint: a value outside `1..=100` is
    /// refused instead of being silently clamped, so an operator that asks for a batch
    /// size the interface does not offer learns about it. Re-running after a crash
    /// continues from the persisted cursor; an operation that is already terminal returns
    /// its stored state without doing work.
    pub fn advance_library_operation(
        &self,
        operation_id: &str,
        now_ms: i64,
        max_media: usize,
    ) -> Result<OperationStatus, StoreError> {
        self.with_library_write(|library| {
            library.advance_library_operation(operation_id, now_ms, max_media)
        })
    }

    /// Current lifecycle state of a Library (`ready`, `reindexing`, ...).
    pub fn library_lifecycle_state(&self, library_id: &str) -> Result<String, StoreError> {
        self.with_library_read(|db| db.library_lifecycle_state(library_id))
    }

    /// Operations left unfinished by a previous process, oldest first.
    ///
    /// Startup recovery advances these before workers accept new Library work.
    pub fn unfinished_library_operations(&self) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.unfinished_library_operations())
    }

    pub fn get_library_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<OperationStatus>, StoreError> {
        self.with_library_read(|db| db.get_library_operation(operation_id))
    }

    /// Read-only projection of one operation, including its resumable cursor.
    ///
    /// [`Store::get_library_operation`] predates the cursor and is already consumed by
    /// three callers, so the richer view is a separate additive read: it never writes.
    pub fn library_operation_view(
        &self,
        operation_id: &str,
    ) -> Result<Option<LibraryOperationView>, StoreError> {
        self.with_library_read(|db| db.library_operation_view(operation_id))
    }

    /// The operation that currently owns this Library, if any is still running.
    ///
    /// `libraries.lifecycle_state = 'reindexing'` is the freeze verdict; this finds the
    /// row that explains it, so a refused write can hand back its `operation_id`.
    pub fn active_library_operation(&self, library_id: &str) -> Result<Option<String>, StoreError> {
        self.with_library_read(|db| db.active_library_operation(library_id))
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
             SET state = ?2, cursor_json = ?3, updated_at_ms = ?4, error = NULL
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

#[path = "connection.rs"]
mod connection;
