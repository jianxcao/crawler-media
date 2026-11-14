//! Bounded progress snapshots for image delivery diagnostics.
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub(super) struct Progress(Mutex<(&'static str, Instant)>);
impl Progress {
    pub(super) fn new(phase: &'static str) -> Arc<Self> {
        Arc::new(Self(Mutex::new((phase, Instant::now()))))
    }
    pub(super) fn set(&self, phase: &'static str) {
        *self.0.lock() = (phase, Instant::now());
    }
    pub(super) fn snapshot(&self) -> (&'static str, u64) {
        let (phase, started) = *self.0.lock();
        (phase, millis(started))
    }
}
pub(super) fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Progress>>> = const { RefCell::new(None) };
}

pub(super) fn set(phase: &'static str) {
    CURRENT.with(|current| {
        if let Some(progress) = current.borrow().as_ref() {
            progress.set(phase);
        }
    });
}

pub(super) fn scope<T>(progress: Arc<Progress>, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<Progress>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|current| *current.borrow_mut() = self.0.take());
        }
    }
    let previous = CURRENT.with(|current| current.replace(Some(progress)));
    let _restore = Restore(previous);
    work()
}
