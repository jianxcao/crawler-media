//! Long-running Media identity operations recorded in `media_operations`.
//!
//! A merge is accepted here (canonical id, pinned revisions, lock conflict,
//! refresh-target decisions, freeze) in `preflight`, then driven through its
//! persisted phases by `apply`.

pub mod apply;
mod preflight;
mod references;

pub(crate) use preflight::{FrozenPair, frozen_of_plan, merge_holder, refresh_target_conflicts};

/// `media_operations.state` of a finished merge.
///
/// Only `succeeded` is terminal: `apply` resumes a `failed` merge from its persisted phase
/// cursor, so that row is still live work.
pub(crate) const SUCCEEDED_STATE: &str = "succeeded";

/// Terminal predicate for retention, derived from the state this module writes.
pub(crate) fn terminal_operation_predicate() -> String {
    format!("state = '{SUCCEEDED_STATE}'")
}
