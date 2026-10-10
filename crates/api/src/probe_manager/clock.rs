use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Millisecond clock used by probe eligibility and retry dispatch.
pub trait ProbeClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Clone, Default)]
pub struct SystemClock;

impl ProbeClock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or_default()
    }
}

/// Test clock. Production dispatch uses `SystemClock`.
#[derive(Clone)]
pub struct FakeClock {
    now_ms: Arc<AtomicI64>,
}

impl FakeClock {
    pub fn new(now_ms: i64) -> Self {
        Self {
            now_ms: Arc::new(AtomicI64::new(now_ms)),
        }
    }

    pub fn set(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::Relaxed);
    }
}

impl ProbeClock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::Relaxed)
    }
}
