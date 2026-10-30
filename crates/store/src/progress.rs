//! Diagnostic progress policy attaches only while a connection is actually leased.
use crate::Store;
use std::{
    cell::RefCell,
    sync::{Arc, Mutex},
};
#[derive(Clone)]
pub(crate) struct Progress {
    pub(crate) interval: i32,
    callback: Arc<Mutex<Box<dyn FnMut() -> bool + Send>>>,
}
impl Progress {
    pub(crate) fn run(&self) -> bool {
        (self.callback.lock().unwrap_or_else(|p| p.into_inner()))()
    }
}
thread_local! { static POLICY:RefCell<Option<Progress>>=const {RefCell::new(None)}; }
pub(crate) fn active() -> Option<Progress> {
    POLICY.with(|p| p.borrow().clone())
}
impl Store {
    #[doc(hidden)]
    pub fn with_library_progress<T>(
        &self,
        interval: i32,
        callback: impl FnMut() -> bool + Send + 'static,
        work: impl FnOnce() -> T,
    ) -> T {
        struct Restore(Option<Progress>);
        impl Drop for Restore {
            fn drop(&mut self) {
                POLICY.with(|p| *p.borrow_mut() = self.0.take());
            }
        }
        let previous = POLICY.with(|p| {
            p.replace(Some(Progress {
                interval,
                callback: Arc::new(Mutex::new(Box::new(callback))),
            }))
        });
        let _restore = Restore(previous);
        work()
    }
}
