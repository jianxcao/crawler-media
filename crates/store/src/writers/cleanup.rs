//! Cancellation applies to business SQL; connection cleanup must still complete.
use crate::library_read::WriterQueryPolicy;
use crate::{QueryBudget, StoreError};
use rusqlite::Connection;
use std::{cell::RefCell, rc::Rc};

pub(super) struct WriterScope<'a> {
    connection: &'a Connection,
    policy: Option<WriterQueryPolicy<'a>>,
    entered_autocommit: bool,
    deferred_errors: Rc<RefCell<Vec<StoreError>>>,
}

impl<'a> WriterScope<'a> {
    pub(super) fn new(
        connection: &'a Connection,
        budget: Option<&QueryBudget>,
        deferred_errors: Rc<RefCell<Vec<StoreError>>>,
    ) -> Result<Self, StoreError> {
        let entered_autocommit = connection.is_autocommit();
        let policy = budget
            .map(|budget| WriterQueryPolicy::install(connection, budget))
            .transpose()?;
        Ok(Self {
            connection,
            policy,
            entered_autocommit,
            deferred_errors,
        })
    }

    pub(super) fn finish(&mut self) -> Result<(), StoreError> {
        // A cancelled progress callback also interrupts COMMIT and ROLLBACK.
        // Reset it before recovering a transaction that failed to unwind.
        drop(self.policy.take());
        if self.entered_autocommit && !self.connection.is_autocommit() {
            self.connection.execute_batch("ROLLBACK")?;
        }
        Ok(())
    }
}

impl Drop for WriterScope<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            self.deferred_errors.borrow_mut().push(error);
        }
    }
}
