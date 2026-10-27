//! Deterministic per-thread work counters used by work-budget tests.

use std::cell::Cell;

thread_local! {
    static USER_SUMMARY_WRITES: Cell<u64> = const { Cell::new(0) };
}

pub(crate) fn count_user_summary_write() {
    USER_SUMMARY_WRITES.with(|c| c.set(c.get() + 1));
}

/// User summary rows recomputed on this thread since start.
pub fn user_summary_writes() -> u64 {
    USER_SUMMARY_WRITES.with(Cell::get)
}
