//! Scan-operation lifecycle tests, split by scenario: admission/scope capture, requeue
//! and reporting, and the failure paths. The Store fixture lives in [`fixture`].

mod admission;
mod failures;
mod fixture;
mod requeue;
#[path = "scope_changes.rs"]
mod scope_changes;

pub(crate) use fixture::{Fixture, MediaKind, ScanOperationStatus};
