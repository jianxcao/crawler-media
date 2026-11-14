//! Integrity check and repair operation for a library.
//!
//! Two-way enumeration:
//! 1. From files/coverage and playback facts to derived rows (library_items, library_units,
//!    library_item_terms, user_library_items).
//! 2. From derived rows back to raw facts (detecting reverse orphans).
//!
//! In repair mode:
//! Re-reduces affected media in write transactions from localized facts.
//! When errors are detected and repair=false, persists `integrity_state='failed'`.
//! When repair succeeds and a final verification passes, clears to `integrity_state='clean'`.

use domain::{LibraryId, MediaId, UserId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{integrity_guard::IntegrityOwner, integrity_query};
use crate::playback::types::OperationStatus;
use crate::query_projection::ProjectionKey;
use crate::{Store, StoreError};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct IntegrityPlan {
    pub repair: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IntegrityReport {
    pub query_projection_media: Vec<MediaId>,
    #[serde(default)]
    pub query_count_mismatch: bool,
    pub missing_items: Vec<MediaId>,
    pub missing_user_items: Vec<(UserId, MediaId)>,
    pub orphan_items: Vec<MediaId>,
    pub orphan_user_items: Vec<(UserId, MediaId)>,
    /// Media whose derived rows (items, units, terms, user rows) differ from what their
    /// raw facts reduce to. Capped at [`REPORT_CAP`]; `issues_found` keeps the true total.
    #[serde(default)]
    pub mismatched_media: Vec<MediaId>,
    /// Exact issue counts per category; the sample lists above are capped.
    #[serde(default)]
    pub totals: IntegrityTotals,
    /// Whether any sample list dropped entries because it reached [`REPORT_CAP`].
    #[serde(default)]
    pub truncated: bool,
}

/// Exact per-category counts behind the capped report samples.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityTotals {
    #[serde(default)]
    pub query_projection_mismatches: usize,
    pub missing_items: usize,
    pub missing_user_items: usize,
    pub orphan_items: usize,
    pub orphan_user_items: usize,
    pub mismatched_media: usize,
}

/// Append a sample while it stays under [`REPORT_CAP`]; count it regardless.
fn sample<T>(list: &mut Vec<T>, total: &mut usize, truncated: &mut bool, value: T) {
    *total += 1;
    if list.len() < REPORT_CAP {
        list.push(value);
    } else {
        *truncated = true;
    }
}

/// Most Media a persisted report lists; the cursor never accumulates the whole Library.
pub const REPORT_CAP: usize = 100;

impl IntegrityReport {
    /// Whether the check found any issue (the bounded issue lists are non-empty).
    pub fn has_issues(&self) -> bool {
        !(self.missing_items.is_empty()
            && self.missing_user_items.is_empty()
            && self.orphan_items.is_empty()
            && self.orphan_user_items.is_empty()
            && self.mismatched_media.is_empty()
            && self.totals.query_projection_mismatches == 0
            && !self.query_count_mismatch)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct IntegrityCursor {
    pub step: u64,
    pub current: Option<paging::MediaPage>,
    #[serde(default)]
    pub last_media_id: Option<String>,
    #[serde(default)]
    pub issues_found: usize,
    #[serde(default)]
    pub issues_repaired: usize,
    /// Media still inconsistent after their repair was re-verified.
    #[serde(default)]
    pub unrepaired: usize,
    #[serde(default)]
    pub report: IntegrityReport,
}

impl Store {
    pub fn begin_library_integrity(
        &self,
        library: LibraryId,
        repair: bool,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        self.with_library_write(|db| {
            db.begin_library_integrity(library, repair, operation_id, now_ms)
        })
    }

    /// Read the integrity report from the operation's cursor.
    pub fn library_integrity_report(
        &self,
        operation_id: &str,
    ) -> Result<IntegrityReport, StoreError> {
        self.with_library_read(|db| db.library_integrity_report(operation_id))
    }
}

fn finish_integrity_batch(
    tx: &Connection,
    owner: &IntegrityOwner,
    library: &str,
    library_id: LibraryId,
    plan: &IntegrityPlan,
    operation_id: &str,
    state: &mut IntegrityCursor,
) -> Result<&'static str, StoreError> {
    if plan.repair && state.unrepaired > 0 {
        return Err(StoreError::Protected(
            "Integrity repair verification failed; owner retained".into(),
        ));
    }
    if integrity_query::finish_counts(tx, owner, library_id, operation_id, plan.repair)? {
        state.report.query_count_mismatch = true;
        state.report.totals.query_projection_mismatches += 1;
        state.issues_found += 1;
        if plan.repair {
            state.issues_repaired += 1;
        }
    }
    // Every Media was compared (and, in repair mode, re-verified) page by page.
    let clean = if plan.repair {
        state.unrepaired == 0
    } else {
        state.issues_found == 0
    };
    tx.execute(
        "UPDATE libraries SET integrity_state = ?2, lifecycle_state='ready' WHERE library_id = ?1",
        params![library, if clean { "clean" } else { "failed" }],
    )?;
    let final_state = if plan.repair && !clean {
        "failed"
    } else {
        "succeeded"
    };
    Ok(final_state)
}

fn read_integrity_cursor(
    tx: &Connection,
    operation_id: &str,
) -> Result<IntegrityCursor, StoreError> {
    let raw: Option<Option<String>> = tx
        .query_row(
            "SELECT cursor_json FROM library_operations WHERE operation_id = ?1",
            params![operation_id],
            |r| r.get(0),
        )
        .optional()?;
    match raw.flatten().filter(|s| !s.trim().is_empty()) {
        Some(s) => serde_json::from_str(&s).map_err(|e| StoreError::Invalid(e.to_string())),
        None => Ok(IntegrityCursor::default()),
    }
}

fn write_integrity_cursor(
    tx: &Connection,
    operation_id: &str,
    cursor: &IntegrityCursor,
    state: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    let json_str = serde_json::to_string(cursor).map_err(|e| StoreError::Invalid(e.to_string()))?;
    tx.execute(
        "UPDATE library_operations SET state = ?2, cursor_json = ?3, updated_at_ms = ?4, error=NULL WHERE operation_id = ?1",
        params![operation_id, state, json_str, now_ms],
    )?;
    Ok(())
}

#[path = "integrity/connection.rs"]
mod connection;
pub(crate) mod paging;
