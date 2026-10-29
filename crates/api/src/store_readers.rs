//! L1 read side: a bounded pool of read-only [`Store`]s (`Store::open_reader`).
//!
//! Request reads ([`crate::http::store_wait::store_read`]) borrow one of these
//! instead of locking the process-wide writer `Store`, so a scan / probe / scrape
//! batch that holds the writer lock (or an open SQLite write transaction) never
//! delays a page. Waiting here only means "all readers are busy reading".

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use store::{Store, StoreError};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Match the eight interactive HTTP workers without allocating an unbounded
/// number of connections (each Store opens four database files).
pub(crate) const STORE_READERS: usize = 8;

pub(crate) struct StoreReaders {
    idle: Mutex<Vec<Store>>,
    returned: Condvar,
    permits: Arc<Semaphore>,
}

impl StoreReaders {
    pub(crate) fn open(writer: &Store, size: usize) -> Result<Self, StoreError> {
        let idle = (0..size.max(1))
            .map(|_| writer.open_reader())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            idle: Mutex::new(idle),
            returned: Condvar::new(),
            permits: Arc::new(Semaphore::new(size.max(1))),
        })
    }

    /// Borrow a reader, waiting at most `wait` for one to come back.
    pub(crate) fn checkout(self: &Arc<Self>, wait: Duration) -> Option<StoreReader> {
        let deadline = Instant::now() + wait;
        let mut idle = self.idle.lock();
        loop {
            if let Ok(permit) = self.permits.clone().try_acquire_owned() {
                let store = idle.pop().expect("a permit owns an idle reader");
                return Some(StoreReader {
                    pool: self.clone(),
                    store: Some(store),
                    permit: Some(permit),
                });
            }
            if Instant::now() >= deadline {
                return None;
            }
            self.returned.wait_until(&mut idle, deadline);
        }
    }

    /// Queue on the async side so cancelled requests do not occupy a blocking
    /// thread or execute stale work after a reader becomes available.
    pub(crate) async fn checkout_async(self: &Arc<Self>, wait: Duration) -> Option<StoreReader> {
        // The acquisition future drops before this guard. Cancellation can
        // return a permit assigned to an unpolled waiter without dropping a
        // StoreReader, so it must also wake synchronous background readers.
        let mut cancelled = CancelledWaiter(Some(self));
        let acquire = self.permits.clone().acquire_owned();
        let permit = tokio::time::timeout(wait, acquire).await.ok()?.ok()?;
        cancelled.0 = None;
        let store = self
            .idle
            .lock()
            .pop()
            .expect("a permit owns an idle reader");
        Some(StoreReader {
            pool: self.clone(),
            store: Some(store),
            permit: Some(permit),
        })
    }
}

struct CancelledWaiter<'a>(Option<&'a StoreReaders>);

impl Drop for CancelledWaiter<'_> {
    fn drop(&mut self) {
        if let Some(pool) = self.0 {
            let _idle = pool.idle.lock();
            pool.returned.notify_all();
        }
    }
}

pub(crate) struct StoreReader {
    pool: Arc<StoreReaders>,
    store: Option<Store>,
    permit: Option<OwnedSemaphorePermit>,
}

impl std::ops::Deref for StoreReader {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.store.as_ref().expect("reader present until drop")
    }
}

impl Drop for StoreReader {
    fn drop(&mut self) {
        if let Some(store) = self.store.take() {
            store.reset_if_in_transaction();
            if store.has_open_transaction() {
                tracing::error!("retiring reader whose transaction could not be rolled back");
                if let Some(permit) = self.permit.take() {
                    permit.forget();
                }
                return;
            }
            // Return the connection before exposing its permit. Keep the mutex
            // through notify so synchronous waiters cannot miss the return.
            let mut idle = self.pool.idle.lock();
            idle.push(store);
            drop(self.permit.take());
            self.pool.returned.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_returned_inside_transaction_is_rolled_back() {
        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let writer = Store::open(tmp.path()).unwrap();
        let readers = Arc::new(StoreReaders::open(&writer, 1).unwrap());
        let reader = readers.checkout(Duration::ZERO).unwrap();
        store::test_support::begin_library_snapshot(&reader).unwrap();
        assert!(reader.library_in_transaction());
        drop(reader);
        let reader = readers.checkout(Duration::ZERO).unwrap();
        assert!(!reader.library_in_transaction());
    }

    #[tokio::test]
    async fn cancelling_an_assigned_async_waiter_wakes_a_synchronous_reader() {
        use std::future::Future;
        use std::task::{Context, Waker};

        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let writer = Store::open(tmp.path().join("data")).unwrap();
        let readers = Arc::new(StoreReaders::open(&writer, 1).unwrap());
        let held = readers.checkout(Duration::ZERO).unwrap();
        let mut waiting = Box::pin(readers.checkout_async(Duration::from_secs(5)));
        let mut context = Context::from_waker(Waker::noop());
        assert!(waiting.as_mut().poll(&mut context).is_pending());
        // The returned permit belongs to the queued future until it is polled
        // again or cancelled; the Store itself is already idle.
        drop(held);

        let started = Instant::now();
        let reader = std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                drop(waiting);
            });
            readers.checkout(Duration::from_secs(2))
        });
        assert!(reader.is_some());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancelled requests must wake background readers when they return capacity"
        );
    }

    #[test]
    fn checkout_waits_for_a_returned_reader_and_times_out_when_none_comes_back() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = Store::open(tmp.path().join("data")).unwrap();
        let readers = Arc::new(StoreReaders::open(&writer, 1).unwrap());
        let first = readers.checkout(Duration::ZERO).expect("one idle reader");
        let started = Instant::now();
        assert!(readers.checkout(Duration::from_millis(50)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(50));
        drop(first);
        assert!(readers.checkout(Duration::ZERO).is_some());
    }
}
