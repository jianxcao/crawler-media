//! Complete SQL work for an explicitly instrumented Store query.
use serde::{Deserialize, Serialize};

mod trace;
pub(crate) use trace::QueryWorkScope;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryWorkStats {
    pub fullscan_steps: u64,
    pub vm_steps: u64,
    /// Public page rows (including the cursor lookahead row).
    pub rows_returned: u64,
    /// All SQL result rows, including authorization, count and facet helpers.
    pub sql_rows: u64,
    pub statements: u64,
}
