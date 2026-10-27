//! Optional store-lock hold tracing for the scan runner.
//!
//! The scan's reader-visible cost is not its total runtime, it is the longest single hold of
//! the process-wide `Store` lock: every bounded reader (heartbeat, playback progress, browse)
//! queues behind one hold. A mean over batches hides that, so the runner records the length of
//! each lock section it takes.
//!
//! Tracing is off by default and thread-local: a scan walks and publishes its chunks on one
//! blocking thread, so a scan-side test turns tracing on around one scan and drains it after.
//! With tracing off, an instrumented section costs one thread-local borrow and no clock read.

use std::cell::RefCell;

/// One measured lock section: the phase name and how long the lock was held.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanHoldSample {
    pub section: &'static str,
    pub micros: u64,
}

thread_local! {
    /// One scan runs its chunks on one thread, so samples belong to that scan and a
    /// concurrently running scan (another test, another request) can never mix in.
    static SAMPLES: RefCell<Option<Vec<ScanHoldSample>>> = const { RefCell::new(None) };
}

/// Start recording lock sections on this thread, discarding anything recorded before.
#[doc(hidden)]
#[allow(dead_code)]
pub fn start_scan_hold_trace() {
    SAMPLES.with(|samples| *samples.borrow_mut() = Some(Vec::new()));
}

/// Stop recording on this thread and return what was recorded.
#[doc(hidden)]
#[allow(dead_code)]
pub fn take_scan_hold_trace() -> Vec<ScanHoldSample> {
    SAMPLES.with(|samples| samples.borrow_mut().take().unwrap_or_default())
}

/// Measure one lock section; the lock must already be held when this is created.
pub(super) fn trace_lock_section(section: &'static str) -> LockSection {
    let started = SAMPLES
        .with(|samples| samples.borrow().as_ref().map(|_| std::time::Instant::now()));
    LockSection { section, started }
}

pub(super) struct LockSection {
    section: &'static str,
    started: Option<std::time::Instant>,
}

impl Drop for LockSection {
    fn drop(&mut self) {
        let Some(started) = self.started else {
            return;
        };
        let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        SAMPLES.with(|samples| {
            if let Some(samples) = samples.borrow_mut().as_mut() {
                samples.push(ScanHoldSample {
                    section: self.section,
                    micros,
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_are_recorded_only_while_tracing() {
        drop(trace_lock_section("off"));
        assert!(take_scan_hold_trace().is_empty(), "tracing is off by default");

        start_scan_hold_trace();
        {
            let _held = trace_lock_section("record");
        }
        let samples = take_scan_hold_trace();
        assert_eq!(samples.len(), 1, "{samples:?}");
        assert_eq!(samples[0].section, "record");
        assert!(take_scan_hold_trace().is_empty(), "taking the trace drains it");
    }
}
