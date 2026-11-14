//! This binary runs one test, with background drivers disabled during fixture setup.
//! Count Rust allocations across the provider's runtime and blocking worker, so the
//! measured scope includes every light candidate, not just the returned snapshot.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct MeasuredAllocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every allocation is delegated unchanged to System; atomics only observe
// requested sizes and never inspect, retain or modify an allocated pointer.
unsafe impl GlobalAlloc for MeasuredAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: GlobalAlloc's caller supplies the valid layout forwarded to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the same pointer/layout contract is forwarded unchanged to System.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        // SAFETY: the caller's allocation and new size are forwarded unchanged.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

pub struct Measurement;
impl Measurement {
    pub fn start() -> Self {
        assert!(!ENABLED.swap(true, Ordering::SeqCst));
        BYTES.store(0, Ordering::SeqCst);
        Self
    }

    pub fn finish(self) -> usize {
        ENABLED.store(false, Ordering::SeqCst);
        BYTES.load(Ordering::SeqCst)
    }
}
impl Drop for Measurement {
    fn drop(&mut self) {
        ENABLED.store(false, Ordering::SeqCst);
    }
}
