//! Read stats for testing/diagnostic usage.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryWorkStats {
    pub fullscan_steps: u64,
    pub vm_steps: u64,
    pub rows_returned: u64,
}
