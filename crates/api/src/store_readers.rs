//! L1 read side: a bounded pool of read-only [`Store`]s (`Store::open_reader`).
//!
//! Request reads ([`crate::http::store_wait::store_read`]) borrow one of these
//! instead of locking the process-wide writer `Store`, so a scan / probe / scrape
//! batch that holds the writer lock (or an open SQLite write transaction) never
//! delays a page. Waiting here only means "all readers are busy reading".

use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use store::{Store, StoreError};

/// Read-only Stores kept per process (each holds one connection per database file).
pub(crate) const STORE_READERS: usize = 4;

pub(crate) struct StoreReaders {
    idle: Mutex<Vec<Store>>,
    returned: Condvar,
}

impl StoreReaders {
    pub(crate) fn open(writer: &Store, size: usize) -> Result<Self, StoreError> {
        let idle = (0..size.max(1))
            .map(|_| writer.open_reader())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            idle: Mutex::new(idle),
            returned: Condvar::new(),
        })
    }

    /// Borrow a reader, waiting at most `wait` for one to come back.
    pub(crate) fn checkout(&self, wait: Duration) -> Option<StoreReader<'_>> {
        let deadline = Instant::now() + wait;
        let mut idle = self.idle.lock();
        loop {
            if let Some(store) = idle.pop() {
                return Some(StoreReader {
                    pool: self,
                    store: Some(store),
                });
            }
            if self.returned.wait_until(&mut idle, deadline).timed_out() && idle.is_empty() {
                return None;
            }
        }
    }
}

pub(crate) struct StoreReader<'a> {
    pool: &'a StoreReaders,
    store: Option<Store>,
}

impl std::ops::Deref for StoreReader<'_> {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.store.as_ref().expect("reader present until drop")
    }
}

impl Drop for StoreReader<'_> {
    fn drop(&mut self) {
        if let Some(store) = self.store.take() {
            self.pool.idle.lock().push(store);
            self.pool.returned.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_waits_for_a_returned_reader_and_times_out_when_none_comes_back() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = Store::open(tmp.path().join("data")).unwrap();
        let readers = StoreReaders::open(&writer, 1).unwrap();
        let first = readers.checkout(Duration::ZERO).expect("one idle reader");
        let started = Instant::now();
        assert!(readers.checkout(Duration::from_millis(50)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(50));
        drop(first);
        assert!(readers.checkout(Duration::ZERO).is_some());
    }
}
