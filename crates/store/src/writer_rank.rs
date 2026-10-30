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
    pub fn enter(rank: WriterRank) -> Self {
        HELD.with(|held| {
            let mut held=held.borrow_mut();
            debug_assert!(held.last().is_none_or(|previous|*previous<rank),
                "writer acquisition must follow app → subscribe → library → cache → jobs; no same-db reentry");
            held.push(rank);
        });
        Self {
            rank,
            _thread: PhantomData,
        }
    }
}
impl Drop for WriterRankGuard {
    fn drop(&mut self) {
        HELD.with(|held| {
            let released = held.borrow_mut().pop();
            debug_assert_eq!(released, Some(self.rank));
        });
    }
}
