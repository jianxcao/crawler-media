use super::*;
use api::fingerprint_job::benchmark::{BenchmarkClock, BenchmarkRun};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub(super) fn run(session: BenchmarkSession) -> BenchmarkRun {
    let _analysis_clock = AnalysisClock::advance(&session);
    session.run().unwrap()
}

pub(super) struct MeasurementClock {
    origin: Instant,
    deadline: Duration,
    resumed: std::sync::Mutex<Option<Instant>>,
    expired: AtomicBool,
}

impl MeasurementClock {
    pub(super) fn new(deadline_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            deadline: Duration::from_millis(deadline_ms),
            resumed: std::sync::Mutex::new(None),
            expired: AtomicBool::new(false),
        })
    }

    pub(super) fn expire(&self) {
        self.expired.store(true, Ordering::SeqCst);
    }

    pub(super) fn resume(&self) {
        *self.resumed.lock().unwrap() = Some(Instant::now());
    }
}

impl BenchmarkClock for MeasurementClock {
    fn now(&self) -> Instant {
        let elapsed = if self.expired.load(Ordering::SeqCst) {
            self.deadline
        } else {
            self.resumed
                .lock()
                .unwrap()
                .map(|started| started.elapsed().min(self.deadline))
                .unwrap_or_default()
        };
        self.origin + elapsed
    }
}

pub(super) struct AnalysisClock {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AnalysisClock {
    pub(super) fn advance(session: &BenchmarkSession) -> Self {
        let clock = Arc::new(api::probe_manager::clock::FakeClock::new(0));
        let manager = session.state.probe.clone();
        manager.set_clock(clock.clone());
        let store = session.state.store().clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut now = 0;
            while !done.load(Ordering::SeqCst) {
                if !store
                    .pending_probe_analysis_keys(None, 1)
                    .unwrap()
                    .is_empty()
                {
                    now += 2_000;
                    clock.set(now);
                    manager.wake_dispatch();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for AnalysisClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
