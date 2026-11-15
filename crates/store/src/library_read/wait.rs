//! One admission queue for blocking and async readers. A ticket never owns a connection
//! until it is polled, so dropping a cancelled waiter cannot strand capacity.
use std::{
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use super::{LibraryReadError, ReaderLease, admission::Admission, pool::Inner};

/// Waiters allowed per connection in the single shared admission queue.
const WAITERS_PER_READER: usize = 4;

pub(super) struct Ticket {
    pool: Arc<Inner>,
    id: u64,
}

impl Ticket {
    pub(super) fn new(pool: Arc<Inner>) -> Result<Self, LibraryReadError> {
        let id = {
            let mut state = pool.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.closed {
                return Err(LibraryReadError::Closed);
            }
            if state.waiters.len() >= pool.capacity().saturating_mul(WAITERS_PER_READER) {
                return Err(LibraryReadError::Busy);
            }
            let id = state.next_ticket;
            state.next_ticket = state.next_ticket.wrapping_add(1);
            state.waiters.push_back((id, None));
            id
        };
        Ok(Self { pool, id })
    }

    pub(super) fn try_acquire(&self) -> Result<Option<ReaderLease>, LibraryReadError> {
        self.reserve()?.map(Admission::open).transpose()
    }

    fn reserve(&self) -> Result<Option<Admission>, LibraryReadError> {
        self.pool.check_identity()?;
        let mut state = self.pool.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return Err(LibraryReadError::Closed);
        }
        if state.waiters.front().map(|w| w.0) != Some(self.id) {
            return Ok(None);
        }
        let epoch = self
            .pool
            .planner_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        if let Some(conn) = state.idle.pop() {
            state.waiters.pop_front();
            drop(state);
            self.pool.wake();
            return Ok(Some(Admission::new(
                Arc::clone(&self.pool),
                epoch,
                Some(conn),
            )));
        }
        if state.total >= self.pool.capacity() {
            return Ok(None);
        }
        state.total += 1;
        state.waiters.pop_front();
        drop(state);
        self.pool.wake();
        Ok(Some(Admission::new(Arc::clone(&self.pool), epoch, None)))
    }

    pub(super) fn blocking(&self, wait: Duration) -> Result<ReaderLease, LibraryReadError> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(lease) = self.try_acquire()? {
                return Ok(lease);
            }
            let state = self.pool.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.closed {
                return Err(LibraryReadError::Closed);
            }
            // Check readiness under the same mutex as wait registration; a return
            // between try_acquire and this lock must not become a lost wakeup.
            if state.waiters.front().map(|w| w.0) == Some(self.id)
                && (!state.idle.is_empty() || state.total < self.pool.capacity())
            {
                drop(state);
                continue;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(LibraryReadError::Busy);
            }
            drop(
                self.pool
                    .returned
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(|p| p.into_inner()),
            );
        }
    }

    pub(super) fn poll(&self, cx: &mut Context<'_>) -> Poll<Result<Admission, LibraryReadError>> {
        // Register first: a return while acquisition checks readiness will wake us.
        {
            let mut state = self.pool.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((_, waker)) = state.waiters.iter_mut().find(|w| w.0 == self.id) {
                *waker = Some(cx.waker().clone());
            }
        }
        match self.reserve() {
            Ok(Some(admission)) => Poll::Ready(Ok(admission)),
            Ok(None) => Poll::Pending,
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.pool
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .waiters
            .retain(|w| w.0 != self.id);
        self.pool.wake();
    }
}
