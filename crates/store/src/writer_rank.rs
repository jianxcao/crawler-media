//! Shared acquisition order for actual database writers, including the API Jobs writer.
use std::{cell::RefCell, marker::PhantomData, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WriterRank {
    App,
    Subscribe,
    Library,
    Cache,
    Jobs,
}

/// A thread-bound rank reservation. The actual mutex must be released first.
#[must_use]
pub struct WriterRankGuard {
    rank: WriterRank,
    _thread: PhantomData<Rc<()>>,
}
thread_local! { static HELD:RefCell<Vec<WriterRank>>=const {RefCell::new(Vec::new())}; }
impl WriterRankGuard {
    /// Reserve the rank before waiting on its mutex, so inversions fail before
    /// they can deadlock. Same-database reentry is rejected as well.
    /// Jobs admission may enclose Store SQL stages, so it cannot be reserved
    /// while any actual database writer is held (including increasing ranks).
    pub fn ensure_unheld(requested: WriterRank) -> Result<(), crate::StoreError> {
        HELD.with(|held| {
            let held = held.borrow();
            if held.is_empty() { return Ok(()); }
            tracing::error!(held = ?*held, ?requested, "writer admission acquired under database writer");
            Err(crate::StoreError::LockOrder { held: held.clone(), requested })
        })
    }

    pub fn enter(rank: WriterRank) -> Result<Self, crate::StoreError> {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if !held.is_empty() {
                tracing::error!(held = ?*held, requested = ?rank, "database writer lock order violation");
                return Err(crate::StoreError::LockOrder { held: held.clone(), requested: rank });
            }
            held.push(rank);
            Ok(Self { rank, _thread: PhantomData })
        })
    }
}
impl Drop for WriterRankGuard {
    fn drop(&mut self) {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if held.last() != Some(&self.rank) {
                tracing::error!(held = ?*held, released = ?self.rank, "database writer rank stack corrupted");
                // An out-of-order guard release is a programming error. Do not
                // pop somebody else's rank and conceal subsequent violations.
                panic!("database writer rank guards released out of order");
            }
            held.pop();
        });
    }
}
