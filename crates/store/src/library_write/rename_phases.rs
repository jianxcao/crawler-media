//! Recording a rename disk receipt in lock-free phases (A7 / C10).
//!
//! Sidecar byte comparison and sidecar moves are filesystem IO; they must not run while a
//! caller holds the Store (the API keeps it behind one mutex). The record is therefore
//! split into Store-only steps (load, decide + commit, mark applied) and value-only IO
//! steps (`observe`, `apply`) that need no Store at all:
//!
//! ```text
//! lock: begin_rename_disk_record → unlock: plan.observe() → lock: decide_rename_disk
//!   → unlock: decided.apply() → lock: finish_rename_disk
//! ```
//!
//! Each Store step re-checks the durable phase, so a phase that raced another recorder is
//! refused or treated as an idempotent replay instead of double-applying sidecars.

use rusqlite::params;

use super::RenameDiskReceipt;
use super::rename_durable::{FileRenameRequest, RenameSidecar, sidecars_json};
use super::rename_sidecars::{
    TargetComparison, apply_sidecar, decide_for_execution, observe_targets,
};
use crate::{Store, StoreError};

/// A planned intent whose sidecar targets still have to be observed (no Store needed).
pub struct RenameSidecarPlan {
    request: FileRenameRequest,
    receipt: RenameDiskReceipt,
}

/// Sidecar targets observed outside every lock and transaction.
pub struct ObservedRenameSidecars {
    plan: RenameSidecarPlan,
    observed: Vec<TargetComparison>,
}

/// Persisted sidecar decisions, ready to be applied on disk (no Store needed).
pub struct DecidedRenameSidecars {
    request: FileRenameRequest,
    sidecars: Vec<RenameSidecar>,
}

/// Sidecars moved on disk; competitor-driven downgrades are carried to the phase change.
pub struct AppliedRenameSidecars {
    request: FileRenameRequest,
    sidecars: Vec<RenameSidecar>,
}

impl RenameSidecarPlan {
    /// Compare every sidecar target's bytes. Filesystem only.
    pub fn observe(self) -> ObservedRenameSidecars {
        let observed = observe_targets(&self.request.sidecars);
        ObservedRenameSidecars {
            plan: self,
            observed,
        }
    }
}

impl DecidedRenameSidecars {
    /// Move the decided sidecars. Filesystem only.
    pub fn apply(mut self) -> Result<AppliedRenameSidecars, StoreError> {
        for side in self.sidecars.iter_mut() {
            apply_sidecar(side)?;
        }
        // `decide_rename_disk` already persisted the receipt identity. Recovery
        // can prove the destination even if we crash before the phase update.
        super::rename_receipt::remove_staging(&self.request)?;
        Ok(AppliedRenameSidecars {
            request: self.request,
            sidecars: self.sidecars,
        })
    }
}

impl Store {
    /// Load the intent for `receipt`. `None` means it is already disk-applied (replay).
    pub fn begin_rename_disk_record(
        &self,
        receipt: &RenameDiskReceipt,
    ) -> Result<Option<RenameSidecarPlan>, StoreError> {
        self.with_library_write(|db| db.begin_rename_disk_record(receipt))
    }

    /// Persist the destination identity and the re-checked sidecar decisions in one short
    /// transaction (CAS on `phase = 'planned'`).
    pub fn decide_rename_disk(
        &self,
        observed: ObservedRenameSidecars,
    ) -> Result<DecidedRenameSidecars, StoreError> {
        self.with_library_write(|db| db.decide_rename_disk(observed))
    }

    /// Mark the intent `disk_applied`; staging cleanup was applied without the Store.
    pub fn finish_rename_disk(
        &self,
        applied: AppliedRenameSidecars,
        now: i64,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.finish_rename_disk(applied, now))
    }
}

#[path = "rename_phases/connection.rs"]
mod connection;
