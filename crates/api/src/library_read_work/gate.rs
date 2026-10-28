//! Bounded async admission for Library reader leases (review T4).
//!
//! One permit per pooled reader connection. A request that finds every
//! connection leased waits here — on the async side, outside the blocking
//! worker, the generic `http_work` budget and the Store lock — until a lease is
//! returned or its absolute deadline passes. The queue is bounded: once
//! `max_waiters` requests are already waiting the read fails fast with Busy.
//! Dropping the waiting future (client gone) removes it from the queue; the
//! permit is held by the blocking worker and released only when it exits.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Waiters allowed per reader connection before a read is rejected as Busy.
const WAITERS_PER_READER: usize = 4;

pub(crate) struct LibraryReadGate {
    permits: Arc<Semaphore>,
    waiting: AtomicUsize,
    max_waiters: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GateError {
    /// The bounded wait queue is full.
    Saturated,
    /// The request deadline passed while queued.
    Deadline,
}

struct Waiting<'a>(&'a AtomicUsize);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl LibraryReadGate {
    pub(crate) fn new(readers: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(readers)),
            waiting: AtomicUsize::new(0),
            max_waiters: readers * WAITERS_PER_READER,
        }
    }

    pub(crate) async fn acquire(
        &self,
        deadline: Instant,
    ) -> Result<OwnedSemaphorePermit, GateError> {
        if let Ok(permit) = self.permits.clone().try_acquire_owned() {
            return Ok(permit);
        }
        let admitted = self
            .waiting
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.max_waiters).then_some(n + 1)
            });
        if admitted.is_err() {
            return Err(GateError::Saturated);
        }
        let _waiting = Waiting(&self.waiting);
        match tokio::time::timeout_at(deadline.into(), self.permits.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_closed)) => Err(GateError::Saturated),
            Err(_) => Err(GateError::Deadline),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn waits_for_release_then_admits() {
        let gate = Arc::new(LibraryReadGate::new(1));
        let held = gate
            .acquire(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        let g = gate.clone();
        let waiter =
            tokio::spawn(async move { g.acquire(Instant::now() + Duration::from_secs(2)).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished());
        drop(held);
        assert!(waiter.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn deadline_and_cancel_leave_queue() {
        let gate = LibraryReadGate::new(1);
        let _held = gate
            .acquire(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        let err = gate
            .acquire(Instant::now() + Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(err, GateError::Deadline);
        assert_eq!(gate.waiting.load(Ordering::Acquire), 0);
        // fill the queue, then one more is rejected without waiting
        let mut pending = Vec::new();
        for _ in 0..WAITERS_PER_READER {
            let mut f = Box::pin(gate.acquire(Instant::now() + Duration::from_secs(5)));
            assert!(futures_poll_once(&mut f).await.is_none());
            pending.push(f);
        }
        assert_eq!(
            gate.acquire(Instant::now() + Duration::from_secs(5))
                .await
                .unwrap_err(),
            GateError::Saturated
        );
        drop(pending);
        assert_eq!(gate.waiting.load(Ordering::Acquire), 0);
    }

    async fn futures_poll_once<F: std::future::Future + Unpin>(f: &mut F) -> Option<F::Output> {
        tokio::time::timeout(Duration::from_millis(5), f).await.ok()
    }
}
