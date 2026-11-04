//! Complete SQL work for an explicitly instrumented Store query.
use serde::{Deserialize, Serialize};

mod trace;
pub(crate) use trace::{QueryWorkCapture, QueryWorkScope};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryWorkStats {
    /// Successful reader admissions for the selected databases in this scope.
    pub reader_checkouts: u64,
    pub writer_wait_ns: u64,
    pub writer_hold_ns: u64,
    /// Only owned `write_tx` calls; excludes raw rusqlite transactions and nested borrows.
    pub covered_owned_write_tx_count: u64,
    pub covered_owned_write_tx_ns: u64,
    /// Maximum duration of a single covered owned transaction, in nanoseconds.
    pub covered_owned_write_tx_max_ns: u64,
    pub fullscan_steps: u64,
    pub vm_steps: u64,
    pub sort_operations: u64,
    /// Public page rows (including the cursor lookahead row).
    pub rows_returned: u64,
    /// All SQL result rows, including authorization, count and facet helpers.
    pub sql_rows: u64,
    /// Logical emitted SQL value bytes: UTF-8 TEXT/BLOB lengths, 8 per numeric, 0 NULL.
    /// This excludes transport, Rust decoding/allocation, and database page volume.
    pub sql_result_value_bytes: u64,
    pub statements: u64,
    /// Sum of SQLite PROFILE durations for statements completed in this scope.
    pub sql_duration_ns: u64,
    /// Files, coverage, Media and user fact rows actually materialized for a page.
    pub hydrated_rows: u64,
}
