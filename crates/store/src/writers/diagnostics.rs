//! Synchronous diagnostics are delivered after writer/rank release. Transaction
//! timing covers only owned `write_tx` calls, including BEGIN and rollback/drop;
//! raw rusqlite transactions and borrowed nested helpers are not counted.
use crate::{Db, Store};
use serde::Serialize;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};

#[doc(hidden)]
#[derive(Clone, Debug, Serialize)]
pub struct WriterSample {
    pub database: Db,
    pub outcome: &'static str,
    pub writer_wait_ns: u64,
    pub writer_hold_ns: u64,
    pub covered_owned_write_tx_count: u64,
    pub covered_owned_write_tx_ns: u64,
    pub covered_owned_write_tx_max_ns: u64,
}
type Observer = Rc<dyn Fn(&WriterSample)>;
struct Delivery {
    sample: WriterSample,
    observers: Vec<Observer>,
}
thread_local! {
    static DELIVERING: Cell<bool> = const { Cell::new(false) };
    static OBSERVERS: RefCell<Vec<Observer>> = const { RefCell::new(Vec::new()) };
    static PENDING: RefCell<Vec<Delivery>> = const { RefCell::new(Vec::new()) };
    static CLEANUP_ERRORS: RefCell<Vec<crate::StoreError>> = const { RefCell::new(Vec::new()) };
    static ACTIVE: RefCell<Vec<Rc<RefCell<WriterSample>>>> = const { RefCell::new(Vec::new()) };
}
fn nanos(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

pub(super) struct Measurement {
    start: Instant,
    acquired: Rc<RefCell<Option<Instant>>>,
    sample: Rc<RefCell<WriterSample>>,
    observers: Vec<Observer>,
}
impl Measurement {
    pub(super) fn new(database: Db) -> Self {
        Self {
            start: Instant::now(),
            observers: OBSERVERS.with(|observers| observers.borrow().clone()),
            acquired: Rc::new(RefCell::new(None)),
            sample: Rc::new(RefCell::new(WriterSample {
                database,
                outcome: "panic",
                writer_wait_ns: 0,
                writer_hold_ns: 0,
                covered_owned_write_tx_count: 0,
                covered_owned_write_tx_ns: 0,
                covered_owned_write_tx_max_ns: 0,
            })),
        }
    }
    pub(super) fn acquired(&mut self) {
        self.sample.borrow_mut().writer_wait_ns = nanos(self.start);
        *self.acquired.borrow_mut() = Some(Instant::now());
        ACTIVE.with(|active| active.borrow_mut().push(self.sample.clone()));
    }
    pub(super) fn hold_end(&self) -> HoldEnd {
        HoldEnd {
            acquired: self.acquired.clone(),
            sample: self.sample.clone(),
        }
    }
    pub(super) fn released(&mut self, outcome: &'static str) {
        let mut sample = self.sample.borrow_mut();
        sample.outcome = outcome;
        if sample.writer_hold_ns == 0 {
            sample.writer_wait_ns = nanos(self.start);
        }
    }
    pub(super) fn emit(&self, errors: Vec<crate::StoreError>) {
        let sample = self.sample.borrow().clone();
        crate::stats::record_writer(&sample);
        CLEANUP_ERRORS.with(|pending| pending.borrow_mut().extend(errors));
        // Reentrant writes keep internal failure/slow-hold reporting; only
        // callback delivery is suppressed so observers cannot recursively fire.
        let observers = if DELIVERING.with(Cell::get) {
            Vec::new()
        } else {
            self.observers.clone()
        };
        PENDING.with(|pending| pending.borrow_mut().push(Delivery { sample, observers }));
        flush_pending();
    }
}

pub(crate) fn flush_pending() {
    // The shared rank includes Jobs and rejected Store admissions. Its owner
    // drops the actual mutex before releasing the rank and invoking this hook.
    if crate::WriterRankGuard::is_held() || DELIVERING.with(Cell::get) {
        return;
    }
    if PENDING.with(|pending| pending.borrow().is_empty()) {
        return;
    }
    DELIVERING.with(|delivering| delivering.set(true));
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            DELIVERING.with(|delivering| delivering.set(false));
        }
    }
    let _restore = Restore;
    loop {
        let deliveries = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
        let errors = CLEANUP_ERRORS.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
        if deliveries.is_empty() && errors.is_empty() {
            break;
        }
        for error in errors {
            tracing::error!(%error, "failed to clean up Store writer transaction");
        }
        for Delivery { sample, observers } in deliveries {
            if sample.writer_hold_ns > 500_000_000 {
                tracing::warn!(?sample, "Store writer hold exceeded 500ms");
            }
            for observer in observers {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(&sample)))
                    .is_err()
                {
                    tracing::error!("Store writer diagnostic observer panicked after release");
                }
            }
        }
    }
}

pub(super) struct HoldEnd {
    acquired: Rc<RefCell<Option<Instant>>>,
    sample: Rc<RefCell<WriterSample>>,
}
impl Drop for HoldEnd {
    fn drop(&mut self) {
        if let Some(start) = self.acquired.borrow_mut().take() {
            self.sample.borrow_mut().writer_hold_ns = nanos(start);
            ACTIVE.with(|active| {
                active.borrow_mut().pop();
            });
        }
    }
}
pub(crate) struct OwnedWriteTimer {
    start: Instant,
    sample: Option<Rc<RefCell<WriterSample>>>,
}
impl OwnedWriteTimer {
    pub(crate) fn start(start: Instant) -> Self {
        Self {
            start,
            sample: ACTIVE.with(|active| active.borrow().last().cloned()),
        }
    }
}
impl Drop for OwnedWriteTimer {
    fn drop(&mut self) {
        if let Some(sample) = &self.sample {
            let mut sample = sample.borrow_mut();
            let elapsed = nanos(self.start);
            sample.covered_owned_write_tx_count =
                sample.covered_owned_write_tx_count.saturating_add(1);
            sample.covered_owned_write_tx_ns =
                sample.covered_owned_write_tx_ns.saturating_add(elapsed);
            sample.covered_owned_write_tx_max_ns =
                sample.covered_owned_write_tx_max_ns.max(elapsed);
        }
    }
}
impl Store {
    #[doc(hidden)]
    pub fn with_writer_diagnostics<T>(
        &self,
        observer: impl Fn(&WriterSample) + 'static,
        work: impl FnOnce() -> T,
    ) -> T {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                OBSERVERS.with(|observers| {
                    observers.borrow_mut().pop();
                });
            }
        }
        OBSERVERS.with(|observers| observers.borrow_mut().push(Rc::new(observer)));
        let _restore = Restore;
        work()
    }
}
