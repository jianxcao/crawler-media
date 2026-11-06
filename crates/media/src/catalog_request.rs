//! Deadline/cancellation shared by synchronous catalog work on a blocking worker.
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::TmdbError;

pub const TMDB_REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Clone)]
pub struct CatalogRequest(Arc<State>);

struct State {
    deadline: Instant,
    cancelled: AtomicBool,
    parent: Option<CatalogRequest>,
    origin: bool,
}

thread_local! {
    static CURRENT: RefCell<Option<CatalogRequest>> = const { RefCell::new(None) };
}

struct Restore(Option<CatalogRequest>);

impl Drop for Restore {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = self.0.take());
    }
}

impl CatalogRequest {
    pub fn new(deadline: Instant) -> Self {
        Self(Arc::new(State {
            deadline,
            cancelled: AtomicBool::new(false),
            parent: None,
            origin: false,
        }))
    }

    /// An upstream operation inherits its caller's lifetime but has its own
    /// shared timeout. Followers must not retry a completed upstream timeout.
    pub fn origin(deadline: Instant) -> Self {
        Self(Arc::new(State {
            deadline,
            cancelled: AtomicBool::new(false),
            parent: Self::current(),
            origin: true,
        }))
    }

    pub fn deadline(&self) -> Instant {
        self.0.parent.as_ref().map_or(self.0.deadline, |parent| {
            self.0.deadline.min(parent.deadline())
        })
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }

    pub fn check(&self) -> Result<(), TmdbError> {
        self.check_at(Instant::now())
    }

    pub fn check_at(&self, now: Instant) -> Result<(), TmdbError> {
        if self.0.cancelled.load(Ordering::Acquire) {
            return Err(TmdbError::Cancelled);
        }
        if let Some(parent) = &self.0.parent {
            parent.check_at(now)?;
        }
        if now >= self.0.deadline {
            return Err(if self.0.origin {
                TmdbError::OriginTimeout
            } else {
                TmdbError::Deadline
            });
        }
        Ok(())
    }

    pub fn current() -> Option<Self> {
        CURRENT.with(|current| current.borrow().clone())
    }

    pub fn scope<T>(&self, work: impl FnOnce() -> T) -> T {
        let _restore = Restore(CURRENT.with(|current| current.replace(Some(self.clone()))));
        work()
    }
}

pub(crate) fn check_current() -> Result<(), TmdbError> {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .map_or(Ok(()), CatalogRequest::check)
    })
}
