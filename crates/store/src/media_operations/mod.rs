//! Long-running Media identity operations recorded in `media_operations`.
//!
//! A merge is accepted here (canonical id, pinned revisions, lock conflict,
//! refresh-target decisions, freeze) in `preflight`, then driven through its
//! persisted phases by `apply`.

mod preflight;
pub mod apply;
mod references;

pub(crate) use preflight::{FrozenPair, frozen_of_plan, merge_holder, refresh_target_conflicts};
